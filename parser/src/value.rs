use std::{error::Error, fmt};

use crate::{XmlAttribute, XmlElement};

/// A typed-value conversion failure that preserves the target type's parse error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum XmlValueError<E> {
    /// The DOM node could not be accessed, for example because its handle is stale.
    Access(crate::XmlDomError),
    /// The lexical value was present but could not be converted to the requested type.
    Parse(E),
}

impl<E: fmt::Display> fmt::Display for XmlValueError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Access(error) => error.fmt(formatter),
            Self::Parse(error) => write!(formatter, "XML value parse error: {error}"),
        }
    }
}

impl<E: Error + 'static> Error for XmlValueError<E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Access(error) => Some(error),
            Self::Parse(error) => Some(error),
        }
    }
}

/// A primitive value with a defined, locale-independent XML lexical representation.
pub trait ToXmlValue {
    /// Returns this value's locale-independent XML lexical representation.
    fn to_xml_value(&self) -> String;
}

macro_rules! integer_xml_values {
    ($($type:ty),+ $(,)?) => {
        $(
            impl ToXmlValue for $type {
                fn to_xml_value(&self) -> String {
                    self.to_string()
                }
            }
        )+
    };
}

integer_xml_values!(
    i8, i16, i32, i64, i128, isize, u8, u16, u32, u64, u128, usize
);

impl ToXmlValue for bool {
    fn to_xml_value(&self) -> String {
        if *self { "true" } else { "false" }.to_owned()
    }
}

impl ToXmlValue for f32 {
    fn to_xml_value(&self) -> String {
        format_float(*self as f64, self.to_string())
    }
}

impl ToXmlValue for f64 {
    fn to_xml_value(&self) -> String {
        format_float(*self, self.to_string())
    }
}

fn format_float(value: f64, finite: String) -> String {
    if value.is_nan() {
        "NaN".to_owned()
    } else if value == f64::INFINITY {
        "INF".to_owned()
    } else if value == f64::NEG_INFINITY {
        "-INF".to_owned()
    } else {
        finite
    }
}

impl XmlAttribute {
    /// Replaces this value using the primitive type's XML lexical representation.
    pub fn set_typed<T: ToXmlValue>(&mut self, value: T) -> &mut Self {
        self.value = value.to_xml_value();
        self
    }
}

impl XmlElement {
    /// Appends an attribute with a typed value without searching existing attributes.
    pub fn append_attribute_typed<T: ToXmlValue>(
        &mut self,
        name: impl Into<String>,
        value: T,
    ) -> Result<&mut XmlAttribute, crate::XmlMutationError> {
        self.append_attribute(name, value.to_xml_value())
    }

    /// Updates or appends an attribute using a typed primitive value.
    pub fn set_attribute_typed<T: ToXmlValue>(
        &mut self,
        name: impl Into<String>,
        value: T,
    ) -> Result<&mut XmlAttribute, crate::XmlMutationError> {
        self.set_attribute(name, value.to_xml_value())
    }

    /// Updates or prepends immediate PCDATA using a typed primitive value.
    pub fn set_text_typed<T: ToXmlValue>(
        &mut self,
        value: T,
    ) -> Result<&mut String, crate::XmlMutationError> {
        self.set_text(value.to_xml_value())
    }
}

#[cfg(test)]
mod tests {
    use crate::{ToXmlValue, XmlElement};

    #[test]
    fn formats_typed_attributes_and_text_deterministically() {
        assert_eq!(i128::MIN.to_xml_value(), i128::MIN.to_string());
        assert_eq!(u128::MAX.to_xml_value(), u128::MAX.to_string());
        assert_eq!(true.to_xml_value(), "true");
        assert_eq!(false.to_xml_value(), "false");
        assert_eq!(1.25_f64.to_xml_value(), "1.25");
        assert_eq!((-0.0_f64).to_xml_value(), "-0");
        assert_eq!(f32::INFINITY.to_xml_value(), "INF");
        assert_eq!(f64::NEG_INFINITY.to_xml_value(), "-INF");
        assert_eq!(f64::NAN.to_xml_value(), "NaN");

        let mut element = XmlElement::new("value").unwrap();
        element.set_attribute_typed("count", 42_u64).unwrap();
        element.attribute_mut("count").unwrap().set_typed(i64::MIN);
        element.set_text_typed(1.5_f32).unwrap();
        assert_eq!(element.attributes[0].value, i64::MIN.to_string());
        assert_eq!(element.text(), Some("1.5"));
    }
}
