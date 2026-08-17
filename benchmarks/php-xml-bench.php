<?php
declare(strict_types=1);

const XML_ELEMENT_NODE = 1;
const XML_TEXT_NODE = 3;
const XML_CDATA_SECTION_NODE = 4;
const XML_PI_NODE = 7;
const XML_COMMENT_NODE = 8;
const XML_PARSE_NOBLANKS = 256;
const XML_PARSE_NONET = 2048;
const XML_PARSE_COMPACT = 65536;

function fail(string $message): never {
    fwrite(STDERR, $message . PHP_EOL);
    exit(1);
}

function parseOptions(array $arguments): array {
    $options = ['runs' => 3, 'iterations' => 1, 'warmup' => 1, 'min_ms' => 300, 'paths' => []];
    for ($index = 1; $index < count($arguments); $index++) {
        $argument = $arguments[$index];
        $mapping = [
            '--runs' => 'runs',
            '--iterations' => 'iterations',
            '--warmup' => 'warmup',
            '--min-duration-ms' => 'min_ms',
        ];
        if (isset($mapping[$argument])) {
            if (++$index >= count($arguments) || !preg_match('/^\d+$/', $arguments[$index])) {
                fail('invalid value for ' . $argument);
            }
            $options[$mapping[$argument]] = (int) $arguments[$index];
        } elseif (str_starts_with($argument, '-')) {
            fail('unknown option: ' . $argument);
        } else {
            $options['paths'][] = $argument;
        }
    }
    if ($options['runs'] < 1 || $options['iterations'] < 1 || $options['warmup'] < 0 ||
        $options['min_ms'] < 1 || count($options['paths']) === 0) {
        fail('runs, iterations, and minimum duration must be positive; warmup must be non-negative');
    }
    return $options;
}

function libxml(): FFI {
    return FFI::cdef(<<<'CDEF'
typedef unsigned char xmlChar;
typedef int xmlElementType;
typedef struct _xmlDoc xmlDoc;
typedef struct _xmlNode xmlNode;
typedef struct _xmlAttr xmlAttr;
struct _xmlNode {
    void *_private;
    xmlElementType type;
    const xmlChar *name;
    xmlNode *children;
    xmlNode *last;
    xmlNode *parent;
    xmlNode *next;
    xmlNode *prev;
    xmlDoc *doc;
    void *ns;
    xmlChar *content;
    xmlAttr *properties;
    void *nsDef;
    void *psvi;
    unsigned short line;
    unsigned short extra;
};
struct _xmlAttr {
    void *_private;
    xmlElementType type;
    const xmlChar *name;
    xmlNode *children;
    xmlNode *last;
    xmlNode *parent;
    xmlAttr *next;
    xmlAttr *prev;
    xmlDoc *doc;
    void *ns;
    int atype;
    void *psvi;
};
xmlDoc *xmlReadMemory(const char *buffer, int size, const char *url, const char *encoding, int options);
xmlNode *xmlDocGetRootElement(const xmlDoc *doc);
void xmlFreeDoc(xmlDoc *cur);
CDEF, 'libxml2.so.2');
}

function isNullPointer(mixed $pointer): bool {
    return $pointer === null || FFI::isNull($pointer);
}

function isSemanticText(mixed $content): bool {
    if (isNullPointer($content)) {
        return false;
    }
    $value = FFI::string(FFI::cast('char *', $content));
    return $value !== '' && strspn($value, " \t\r\n") !== strlen($value);
}

function countElement(FFI\CData $node, array &$counts): void {
    $counts['elements']++;
    $counts['nodes']++;

    $attribute = $node->properties;
    while (!isNullPointer($attribute)) {
        $counts['attributes']++;
        $attribute = $attribute->next;
    }

    $child = $node->children;
    while (!isNullPointer($child)) {
        $type = (int) $child->type;
        if ($type === XML_ELEMENT_NODE) {
            countElement($child, $counts);
        } elseif (($type === XML_TEXT_NODE || $type === XML_CDATA_SECTION_NODE) &&
                  isSemanticText($child->content)) {
            $counts['nodes']++;
        } elseif ($type === XML_PI_NODE || $type === XML_COMMENT_NODE) {
            $counts['nodes']++;
        }
        $child = $child->next;
    }
}

function parseDocument(FFI $xml, string $input, string $path): FFI\CData {
    $document = $xml->xmlReadMemory(
        $input,
        strlen($input),
        $path,
        null,
        XML_PARSE_NOBLANKS | XML_PARSE_NONET | XML_PARSE_COMPACT,
    );
    if (FFI::isNull($document)) {
        fail('XML parse failed: ' . $path);
    }
    return $document;
}

function runSample(FFI $xml, string $input, string $path, int $iterations, int $warmup): array {
    for ($index = 0; $index < $warmup; $index++) {
        $document = parseDocument($xml, $input, $path);
        $counts = ['elements' => 0, 'attributes' => 0, 'nodes' => 0];
        countElement($xml->xmlDocGetRootElement($document), $counts);
        $xml->xmlFreeDoc($document);
    }

    $parseNanoseconds = 0;
    $countNanoseconds = 0;
    $counts = ['elements' => 0, 'attributes' => 0, 'nodes' => 0];
    for ($index = 0; $index < $iterations; $index++) {
        $started = hrtime(true);
        $document = parseDocument($xml, $input, $path);
        $parseNanoseconds += hrtime(true) - $started;

        $started = hrtime(true);
        $counts = ['elements' => 0, 'attributes' => 0, 'nodes' => 0];
        countElement($xml->xmlDocGetRootElement($document), $counts);
        $countNanoseconds += hrtime(true) - $started;
        $xml->xmlFreeDoc($document);
    }
    return [
        'iterations' => $iterations,
        'parse_ms' => $parseNanoseconds / $iterations / 1_000_000,
        'count_ms' => $countNanoseconds / $iterations / 1_000_000,
        'counts' => $counts,
    ];
}

function calibrate(FFI $xml, string $input, string $path, int $initial, int $minimumMs): int {
    $iterations = $initial;
    while (true) {
        $sample = runSample($xml, $input, $path, $iterations, 0);
        $measuredMs = ($sample['parse_ms'] + $sample['count_ms']) * $iterations;
        if ($measuredMs >= $minimumMs) {
            return $iterations;
        }
        $multiplier = max(2, (int) ceil($minimumMs / max($measuredMs, 0.001)));
        $iterations *= $multiplier;
    }
}

function memoryKb(): array {
    $values = ['VmRSS' => 0, 'VmHWM' => 0];
    $status = @file('/proc/self/status', FILE_IGNORE_NEW_LINES) ?: [];
    foreach ($status as $line) {
        if (preg_match('/^(VmRSS|VmHWM):\s+(\d+)/', $line, $match)) {
            $values[$match[1]] = (int) $match[2];
        }
    }
    return [$values['VmRSS'], $values['VmHWM']];
}

function mibPerSecond(int $bytes, float $milliseconds): float {
    return $milliseconds > 0 ? $bytes / 1_048_576 / ($milliseconds / 1_000) : 0.0;
}

$options = parseOptions($argv);
$xml = libxml();
echo "file\tmode\titer\twarmup\tbytes\tread_ms\tparse_ms\tcount_ms\ttotal_ms\t" .
     "parse_mib_s\ttotal_mib_s\trss_kb\thwm_kb\telements\tattributes\tnodes\n";

foreach ($options['paths'] as $path) {
    $started = hrtime(true);
    $input = file_get_contents($path);
    if ($input === false) {
        fail('read failed: ' . $path);
    }
    $readMs = (hrtime(true) - $started) / 1_000_000;
    $iterations = calibrate($xml, $input, $path, $options['iterations'], $options['min_ms']);
    $best = null;
    for ($run = 0; $run < $options['runs']; $run++) {
        $sample = runSample($xml, $input, $path, $iterations, $options['warmup']);
        if ($best === null || $sample['parse_ms'] + $sample['count_ms'] <
            $best['parse_ms'] + $best['count_ms']) {
            $best = $sample;
        }
    }
    [$rssKb, $hwmKb] = memoryKb();
    $parserMs = $best['parse_ms'] + $best['count_ms'];
    echo implode("\t", [
        $path,
        'php-libxml2-compact-walk',
        $best['iterations'],
        $options['warmup'],
        strlen($input),
        number_format($readMs, 3, '.', ''),
        number_format($best['parse_ms'], 3, '.', ''),
        number_format($best['count_ms'], 3, '.', ''),
        number_format($readMs + $parserMs, 3, '.', ''),
        number_format(mibPerSecond(strlen($input), $best['parse_ms']), 1, '.', ''),
        number_format(mibPerSecond(strlen($input), $readMs + $parserMs), 1, '.', ''),
        $rssKb,
        $hwmKb,
        $best['counts']['elements'],
        $best['counts']['attributes'],
        $best['counts']['nodes'],
    ]) . PHP_EOL;
}
