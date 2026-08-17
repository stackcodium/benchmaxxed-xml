#include <algorithm>
#include <chrono>
#include <cstdint>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <sstream>
#include <stdexcept>
#include <string>
#include <vector>

#include "pugixml.hpp"

namespace {

struct Options {
    int runs = 3;
    int iterations = 1;
    int warmup = 1;
    int min_duration_ms = 300;
    std::string mode = "pugixml-default";
    std::string workload;
    std::string emit_path;
    int edits = 100;
    unsigned int flags = pugi::parse_default;
    std::vector<std::string> paths;
};

struct Counts {
    std::uint64_t elements = 0;
    std::uint64_t attributes = 0;
    std::uint64_t nodes = 0;
    std::uint64_t checksum = 0;
};

struct Sample {
    int iterations = 0;
    double parse_ms = 0.0;
    double count_ms = 0.0;
    Counts counts;
};

struct MutationSample {
    int iterations = 0;
    double parse_ms = 0.0;
    double first_edit_ms = 0.0;
    double mutate_ms = 0.0;
    double walk_ms = 0.0;
    double serialize_ms = 0.0;
    Counts counts;
    std::uint64_t output_bytes = 0;
    std::uint64_t output_checksum = 0;
    std::uint64_t selected = 0;

    double end_to_end_ms() const {
        return parse_ms + first_edit_ms + mutate_ms + walk_ms + serialize_ms;
    }
};

std::string usage() {
    return "usage: pugixml_bench [--runs N] [--iterations N] [--warmup N] "
           "[--min-duration-ms N] [--mode default|ws-pcdata|minimal|full] "
           "[--workload parse-walk|xpath-query|sparse-edit|repeated-mutation|structural-edit|retained-reorder|document-build] [--edits N] "
           "[--emit FILE] XML_FILE...";
}

bool parse_int(const std::string& value, int& out) {
    std::istringstream stream(value);
    stream >> out;
    return stream && stream.eof() && out > 0;
}

bool parse_nonnegative_int(const std::string& value, int& out) {
    std::istringstream stream(value);
    stream >> out;
    return stream && stream.eof() && out >= 0;
}

std::string read_file(const std::string& path) {
    std::ifstream file(path, std::ios::binary);
    if (!file) {
        throw std::runtime_error("open failed");
    }
    std::ostringstream buffer;
    buffer << file.rdbuf();
    return buffer.str();
}

Counts count_node(const pugi::xml_node& node) {
    Counts counts;
    if (node.type() == pugi::node_element) {
        counts.elements += 1;
        counts.checksum += std::char_traits<char>::length(node.name());
        for (pugi::xml_attribute attr: node.attributes()) {
            counts.attributes += 1;
            counts.checksum += std::char_traits<char>::length(attr.name());
            counts.checksum += std::char_traits<char>::length(attr.value());
        }
    } else {
        counts.checksum += std::char_traits<char>::length(node.value());
    }

    counts.nodes += 1;
    for (pugi::xml_node child: node.children()) {
        Counts child_counts = count_node(child);
        counts.elements += child_counts.elements;
        counts.attributes += child_counts.attributes;
        counts.nodes += child_counts.nodes;
        counts.checksum += child_counts.checksum;
    }

    return counts;
}

Counts count_document(const pugi::xml_document& document) {
    Counts counts;
    for (pugi::xml_node child: document.children()) {
        // The Rust owned DOM deliberately has no representation for whitespace
        // outside the document element. Keep parse_ws_pcdata enabled for the
        // element tree, but exclude that unrepresentable document-level node
        // from the equivalent complete-owned-DOM walk.
        if (child.type() == pugi::node_pcdata &&
            std::all_of(child.value(), child.value() + std::char_traits<char>::length(child.value()),
                        [](char value) { return value == ' ' || value == '\t' || value == '\r' || value == '\n'; })) {
            continue;
        }
        Counts child_counts = count_node(child);
        counts.elements += child_counts.elements;
        counts.attributes += child_counts.attributes;
        counts.nodes += child_counts.nodes;
        counts.checksum += child_counts.checksum;
    }
    return counts;
}

void set_attribute(pugi::xml_node node, const char* name, const std::string& value) {
    pugi::xml_attribute attribute = node.attribute(name);
    if (!attribute) {
        attribute = node.append_attribute(name);
    }
    if (!attribute || !attribute.set_value(value.c_str())) {
        throw std::runtime_error("attribute mutation failed");
    }
}

void sparse_edit(pugi::xml_document& document) {
    pugi::xml_node root = document.document_element();
    if (!root) {
        throw std::runtime_error("document element missing");
    }
    set_attribute(root, "bench-sparse", "1");
}

void mutation_batch(pugi::xml_document& document, int iteration) {
    pugi::xml_node root = document.document_element();
    if (!root) {
        throw std::runtime_error("document element missing");
    }
    std::string value = std::to_string(iteration);
    set_attribute(root, "bench-iteration", value);
    pugi::xml_node added = root.append_child("mutation-probe");
    if (!added) {
        throw std::runtime_error("element append failed");
    }
    set_attribute(added, "iteration", value);
    pugi::xml_node text = added.append_child(pugi::node_pcdata);
    if (!text || !text.set_value("initial") || !text.set_value(value.c_str())) {
        throw std::runtime_error("text mutation failed");
    }
    if (!root.remove_child(added)) {
        throw std::runtime_error("node removal failed");
    }
}

void relocate_last_element_to_front(pugi::xml_document& document) {
    pugi::xml_node root = document.document_element();
    for (pugi::xml_node child = root.last_child(); child; child = child.previous_sibling()) {
        if (child.type() == pugi::node_element) {
            if (!root.prepend_move(child)) {
                throw std::runtime_error("subtree relocation failed");
            }
            return;
        }
    }
}

void structural_edit(pugi::xml_document& document) {
    pugi::xml_node root = document.document_element();
    pugi::xml_attribute first_attribute = root.prepend_attribute("bench-first");
    if (!first_attribute || !first_attribute.set_value("1")) {
        throw std::runtime_error("ordered attribute mutation failed");
    }
    set_attribute(root, "bench-mode", "structural");

    pugi::xml_node first;
    pugi::xml_node second;
    for (pugi::xml_node child: root.children()) {
        if (child.type() != pugi::node_element) {
            continue;
        }
        if (!first) {
            first = child;
        } else {
            second = child;
            break;
        }
    }
    if (first && second) {
        pugi::xml_node copied = root.append_copy(first);
        if (!copied || !copied.set_name("bench-copy")) {
            throw std::runtime_error("subtree copy or rename failed");
        }
        for (pugi::xml_node child = first.last_child(); child; child = child.previous_sibling()) {
            if (child.type() == pugi::node_element) {
                if (!second.prepend_move(child)) {
                    throw std::runtime_error("cross-parent subtree move failed");
                }
                break;
            }
        }
    }
    pugi::xml_node comment = root.append_child(pugi::node_comment);
    pugi::xml_node pi = root.append_child(pugi::node_pi);
    if (!comment || !comment.set_value("bench-structural") || !pi || !pi.set_name("bench") ||
        !pi.set_value("complete")) {
        throw std::runtime_error("miscellaneous node append failed");
    }
}

std::size_t retained_handle_reorder(pugi::xml_document& document, int limit) {
    pugi::xml_node root = document.document_element();
    std::vector<pugi::xml_node> retained;
    retained.reserve(static_cast<std::size_t>(limit));
    for (pugi::xml_node child: root.children()) {
        if (child.type() == pugi::node_element) {
            retained.push_back(child);
            if (retained.size() == static_cast<std::size_t>(limit)) {
                break;
            }
        }
    }
    std::reverse(retained.begin(), retained.end());
    for (pugi::xml_node node: retained) {
        if (!root.append_move(node)) {
            throw std::runtime_error("retained-handle reorder failed");
        }
    }
    return retained.size();
}

void build_document(pugi::xml_document& document, int elements) {
    pugi::xml_node root = document.document_element();
    for (int index = 0; index < elements; ++index) {
        pugi::xml_node item = root.append_child("item");
        set_attribute(item, "id", std::to_string(index));
        set_attribute(item, "enabled", index % 2 == 0 ? "true" : "false");
        pugi::xml_node name = item.append_child("name");
        pugi::xml_node text = name.append_child(pugi::node_pcdata);
        std::string value = "item-" + std::to_string(index);
        if (!item || !name || !text || !text.set_value(value.c_str())) {
            throw std::runtime_error("document construction failed");
        }
    }
}

struct CountingWriter: pugi::xml_writer {
    std::uint64_t bytes = 0;
    std::uint64_t checksum = 0;

    void write(const void* data, std::size_t size) override {
        static_cast<void>(data);
        bytes += size;
    }
};

MutationSample run_mutation_once(const std::string& input, const Options& options) {
    using clock = std::chrono::steady_clock;
    MutationSample sample;
    sample.iterations = 1;
    pugi::xml_document document;

    auto parse_start = clock::now();
    pugi::xml_parse_result result;
    if (options.workload == "document-build") {
        if (!document.append_child("catalog")) {
            throw std::runtime_error("document root construction failed");
        }
    } else {
        result = document.load_buffer(input.data(), input.size(), options.flags);
    }
    auto parse_end = clock::now();
    if (options.workload != "document-build" && !result) {
        throw std::runtime_error(result.description());
    }
    sample.parse_ms = std::chrono::duration<double, std::milli>(parse_end - parse_start).count();

    if (options.workload == "xpath-query") {
        auto start = clock::now();
        sample.selected = document.select_nodes("//member[@name]").size();
        sample.mutate_ms = std::chrono::duration<double, std::milli>(clock::now() - start).count();
    } else if (options.workload == "sparse-edit") {
        auto start = clock::now();
        sparse_edit(document);
        sample.first_edit_ms = std::chrono::duration<double, std::milli>(clock::now() - start).count();
    } else if (options.workload == "repeated-mutation") {
        auto first_start = clock::now();
        mutation_batch(document, 0);
        sample.first_edit_ms = std::chrono::duration<double, std::milli>(clock::now() - first_start).count();
        auto mutate_start = clock::now();
        for (int iteration = 1; iteration < options.edits; ++iteration) {
            mutation_batch(document, iteration);
        }
        relocate_last_element_to_front(document);
        sample.mutate_ms = std::chrono::duration<double, std::milli>(clock::now() - mutate_start).count();
    } else if (options.workload == "structural-edit") {
        auto start = clock::now();
        structural_edit(document);
        sample.mutate_ms = std::chrono::duration<double, std::milli>(clock::now() - start).count();
    } else if (options.workload == "retained-reorder") {
        auto start = clock::now();
        sample.selected = retained_handle_reorder(document, options.edits);
        sample.mutate_ms = std::chrono::duration<double, std::milli>(clock::now() - start).count();
    } else if (options.workload == "document-build") {
        auto start = clock::now();
        build_document(document, options.edits);
        sample.mutate_ms = std::chrono::duration<double, std::milli>(clock::now() - start).count();
    }

    auto walk_start = clock::now();
    sample.counts = count_document(document);
    sample.walk_ms = std::chrono::duration<double, std::milli>(clock::now() - walk_start).count();

    if (options.workload == "repeated-mutation" || options.workload == "structural-edit" ||
        options.workload == "retained-reorder" || options.workload == "document-build") {
        CountingWriter writer;
        auto serialize_start = clock::now();
        unsigned int format = pugi::format_raw;
        if (options.workload == "document-build") {
            format |= pugi::format_no_declaration;
        }
        document.save(writer, "", format, pugi::encoding_utf8);
        sample.serialize_ms = std::chrono::duration<double, std::milli>(clock::now() - serialize_start).count();
        sample.output_bytes = writer.bytes;
        sample.output_checksum = writer.checksum;
    }
    return sample;
}

MutationSample run_mutation_sample(const std::string& input, const Options& options, int iterations) {
    for (int index = 0; index < options.warmup; ++index) {
        run_mutation_once(input, options);
    }
    MutationSample total;
    total.iterations = iterations;
    for (int index = 0; index < iterations; ++index) {
        MutationSample sample = run_mutation_once(input, options);
        total.parse_ms += sample.parse_ms;
        total.first_edit_ms += sample.first_edit_ms;
        total.mutate_ms += sample.mutate_ms;
        total.walk_ms += sample.walk_ms;
        total.serialize_ms += sample.serialize_ms;
        total.counts = sample.counts;
        total.output_bytes = sample.output_bytes;
        total.output_checksum = sample.output_checksum;
        total.selected = sample.selected;
    }
    total.parse_ms /= iterations;
    total.first_edit_ms /= iterations;
    total.mutate_ms /= iterations;
    total.walk_ms /= iterations;
    total.serialize_ms /= iterations;
    return total;
}

int calibrate_mutation(const std::string& input, const Options& options) {
    int iterations = options.iterations;
    while (true) {
        MutationSample sample = run_mutation_sample(input, options, iterations);
        double measured_ms = sample.end_to_end_ms() * iterations;
        if (measured_ms >= options.min_duration_ms) {
            return iterations;
        }
        double scale = measured_ms <= 0.0 ? 10.0 : options.min_duration_ms / measured_ms;
        int multiplier = std::max(2, static_cast<int>(scale + 0.999));
        if (iterations > (1 << 28) / multiplier) {
            return iterations;
        }
        iterations *= multiplier;
    }
}

void prepare_mutation_document(pugi::xml_document& document, const std::string& input, const Options& options) {
    if (options.workload == "document-build") {
        if (!document.append_child("catalog")) {
            throw std::runtime_error("document root construction failed");
        }
    } else {
        pugi::xml_parse_result result = document.load_buffer(input.data(), input.size(), options.flags);
        if (!result) {
            throw std::runtime_error(result.description());
        }
    }
    if (options.workload == "sparse-edit") {
        sparse_edit(document);
    } else if (options.workload == "repeated-mutation") {
        for (int iteration = 0; iteration < options.edits; ++iteration) {
            mutation_batch(document, iteration);
        }
        relocate_last_element_to_front(document);
    } else if (options.workload == "structural-edit") {
        structural_edit(document);
    } else if (options.workload == "retained-reorder") {
        retained_handle_reorder(document, options.edits);
    } else if (options.workload == "document-build") {
        build_document(document, options.edits);
    }
}

Sample run_sample(const std::string& input, const Options& options, int iterations) {
    using clock = std::chrono::steady_clock;

    pugi::xml_document warm_doc;
    for (int i = 0; i < options.warmup; ++i) {
        pugi::xml_parse_result result = warm_doc.load_buffer(input.data(), input.size(), options.flags);
        if (!result) {
            throw std::runtime_error(result.description());
        }
        warm_doc.reset();
    }

    Counts counts;
    double parse_ms = 0.0;
    double count_ms = 0.0;
    pugi::xml_document document;
    for (int i = 0; i < iterations; ++i) {
        auto parse_start = clock::now();
        document.reset();
        pugi::xml_parse_result result = document.load_buffer(input.data(), input.size(), options.flags);
        if (!result) {
            throw std::runtime_error(result.description());
        }
        auto parse_end = clock::now();
        parse_ms += std::chrono::duration<double, std::milli>(parse_end - parse_start).count();

        auto count_start = clock::now();
        counts = count_document(document);
        auto count_end = clock::now();
        count_ms += std::chrono::duration<double, std::milli>(count_end - count_start).count();
    }

    return Sample{
        iterations,
        parse_ms / iterations,
        count_ms / iterations,
        counts,
    };
}

int calibrate(const std::string& input, const Options& options) {
    int iterations = options.iterations;
    while (true) {
        Sample sample = run_sample(input, options, iterations);
        double measured_ms = (sample.parse_ms + sample.count_ms) * iterations;
        if (measured_ms >= options.min_duration_ms) {
            return iterations;
        }

        double scale = measured_ms <= 0.0 ? 10.0 : options.min_duration_ms / measured_ms;
        int multiplier = std::max(2, static_cast<int>(scale + 0.999));
        if (iterations > (1 << 28) / multiplier) {
            return iterations;
        }
        iterations *= multiplier;
    }
}

std::uint64_t current_rss_kb() {
    std::ifstream status("/proc/self/status");
    std::string key;
    while (status >> key) {
        if (key == "VmRSS:") {
            std::uint64_t value = 0;
            std::string unit;
            status >> value >> unit;
            return value;
        }
        std::string rest;
        std::getline(status, rest);
    }
    return 0;
}

std::uint64_t high_water_rss_kb() {
    std::ifstream status("/proc/self/status");
    std::string key;
    while (status >> key) {
        if (key == "VmHWM:") {
            std::uint64_t value = 0;
            std::string unit;
            status >> value >> unit;
            return value;
        }
        std::string rest;
        std::getline(status, rest);
    }
    return current_rss_kb();
}

double mib_per_second(std::uint64_t bytes, double milliseconds) {
    if (milliseconds <= 0.0) {
        return 0.0;
    }
    return (static_cast<double>(bytes) / (1024.0 * 1024.0)) / (milliseconds / 1000.0);
}

std::string basename_no_extension(const std::string& path) {
    std::size_t slash = path.find_last_of('/');
    std::size_t start = slash == std::string::npos ? 0 : slash + 1;
    std::size_t dot = path.find_last_of('.');
    if (dot == std::string::npos || dot < start) {
        dot = path.size();
    }
    return path.substr(start, dot - start);
}

void run_mutation_benchmark(const Options& options) {
    std::cout
        << "file\tparser\tworkload\tedits\titer\twarmup\tbytes\tparse_ms\tfirst_edit_ms\tmutate_ms\twalk_ms\tserialize_ms\tend_to_end_ms\tmib_s\trss_kb\thwm_kb\telements\tattributes\tnodes\tselected\toutput_bytes\toutput_checksum\n";
    for (const std::string& path: options.paths) {
        std::string input = read_file(path);
        int iterations = calibrate_mutation(input, options);
        MutationSample best;
        bool has_best = false;
        for (int run = 0; run < options.runs; ++run) {
            MutationSample sample = run_mutation_sample(input, options, iterations);
            if (!has_best || sample.end_to_end_ms() < best.end_to_end_ms()) {
                best = sample;
                has_best = true;
            }
        }
        std::cout << path << "\tpugixml\t"
                  << options.workload << '\t'
                  << options.edits << '\t'
                  << iterations << '\t'
                  << options.warmup << '\t'
                  << input.size() << '\t'
                  << std::fixed << std::setprecision(3)
                  << best.parse_ms << '\t'
                  << best.first_edit_ms << '\t'
                  << best.mutate_ms << '\t'
                  << best.walk_ms << '\t'
                  << best.serialize_ms << '\t'
                  << best.end_to_end_ms() << '\t'
                  << std::setprecision(1)
                  << mib_per_second(input.size(), best.end_to_end_ms()) << '\t'
                  << current_rss_kb() << '\t'
                  << high_water_rss_kb() << '\t'
                  << best.counts.elements << '\t'
                  << best.counts.attributes << '\t'
                  << best.counts.nodes << '\t'
                  << best.selected << '\t'
                  << best.output_bytes << '\t'
                  << best.output_checksum << '\n';

        if (!options.emit_path.empty()) {
            if (options.paths.size() != 1) {
                throw std::runtime_error("--emit requires exactly one XML input");
            }
            pugi::xml_document document;
            prepare_mutation_document(document, input, options);
            unsigned int format = pugi::format_raw;
            if (options.workload == "document-build") {
                format |= pugi::format_no_declaration;
            }
            if (!document.save_file(
                    options.emit_path.c_str(), "", format, pugi::encoding_utf8)) {
                throw std::runtime_error("failed to emit mutated XML");
            }
        }
    }
}

Options parse_options(int argc, char** argv) {
    Options options;
    for (int i = 1; i < argc; ++i) {
        std::string arg = argv[i];
        auto need_value = [&](int& out) {
            if (++i >= argc || !parse_int(argv[i], out)) {
                throw std::runtime_error(usage());
            }
        };
        auto need_nonnegative_value = [&](int& out) {
            if (++i >= argc || !parse_nonnegative_int(argv[i], out)) {
                throw std::runtime_error(usage());
            }
        };

        if (arg == "--runs") {
            need_value(options.runs);
        } else if (arg == "--iterations") {
            need_value(options.iterations);
        } else if (arg == "--warmup") {
            need_nonnegative_value(options.warmup);
        } else if (arg == "--min-duration-ms") {
            need_value(options.min_duration_ms);
        } else if (arg == "--edits") {
            need_value(options.edits);
        } else if (arg == "--workload") {
            if (++i >= argc) {
                throw std::runtime_error(usage());
            }
            options.workload = argv[i];
            if (options.workload != "parse-walk" && options.workload != "xpath-query" &&
                options.workload != "sparse-edit" &&
                options.workload != "repeated-mutation" && options.workload != "structural-edit" &&
                options.workload != "retained-reorder" &&
                options.workload != "document-build") {
                throw std::runtime_error(usage());
            }
            options.mode = "pugixml-mutable";
            options.flags = pugi::parse_full | pugi::parse_ws_pcdata;
        } else if (arg == "--emit") {
            if (++i >= argc) {
                throw std::runtime_error(usage());
            }
            options.emit_path = argv[i];
        } else if (arg == "--mode") {
            if (++i >= argc) {
                throw std::runtime_error(usage());
            }
            std::string mode = argv[i];
            if (mode == "default") {
                options.mode = "pugixml-default";
                options.flags = pugi::parse_default;
            } else if (mode == "ws-pcdata") {
                options.mode = "pugixml-ws-pcdata";
                options.flags = pugi::parse_default | pugi::parse_ws_pcdata;
            } else if (mode == "minimal") {
                options.mode = "pugixml-minimal";
                options.flags = pugi::parse_minimal;
            } else if (mode == "full") {
                options.mode = "pugixml-full-ws-pcdata";
                options.flags = pugi::parse_full | pugi::parse_ws_pcdata;
            } else {
                throw std::runtime_error(usage());
            }
        } else if (!arg.empty() && arg[0] == '-') {
            throw std::runtime_error(usage());
        } else {
            options.paths.push_back(arg);
        }
    }

    if (options.paths.empty()) {
        throw std::runtime_error(usage());
    }

    return options;
}

} // namespace

int main(int argc, char** argv) {
    try {
        Options options = parse_options(argc, argv);
        if (!options.workload.empty()) {
            run_mutation_benchmark(options);
            return 0;
        }
        std::cout
            << "file\tmode\titer\twarmup\tbytes\tread_ms\tparse_ms\tcount_ms\ttotal_ms\tparse_mib_s\ttotal_mib_s\trss_kb\thwm_kb\telements\tattributes\tnodes\n";

        for (const std::string& path: options.paths) {
            using clock = std::chrono::steady_clock;
            auto read_start = clock::now();
            std::string input = read_file(path);
            auto read_end = clock::now();
            double read_ms = std::chrono::duration<double, std::milli>(read_end - read_start).count();

            int iterations = calibrate(input, options);
            Sample best;
            bool has_best = false;
            for (int i = 0; i < options.runs; ++i) {
                Sample sample = run_sample(input, options, iterations);
                if (!has_best || sample.parse_ms + sample.count_ms < best.parse_ms + best.count_ms) {
                    best = sample;
                    has_best = true;
                }
            }

            double total_ms = read_ms + best.parse_ms + best.count_ms;
            std::cout << path << '\t'
                      << options.mode << '\t'
                      << best.iterations << '\t'
                      << options.warmup << '\t'
                      << input.size() << '\t'
                      << std::fixed << std::setprecision(3)
                      << read_ms << '\t'
                      << best.parse_ms << '\t'
                      << best.count_ms << '\t'
                      << total_ms << '\t'
                      << std::setprecision(1)
                      << mib_per_second(input.size(), best.parse_ms) << '\t'
                      << mib_per_second(input.size(), total_ms) << '\t'
                      << current_rss_kb() << '\t'
                      << high_water_rss_kb() << '\t'
                      << best.counts.elements << '\t'
                      << best.counts.attributes << '\t'
                      << best.counts.nodes << '\n';
        }
    } catch (const std::exception& error) {
        std::cerr << error.what() << '\n';
        return 1;
    }

    return 0;
}
