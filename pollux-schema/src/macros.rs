#[macro_export]
/// Generate an `Option<T>` deserializer for API fields that accept string or array forms.
///
/// The generated function accepts:
/// - missing field via `#[serde(default)]` as `None`
/// - JSON `null` as `None`
/// - JSON string via the supplied mapper expression
/// - JSON array via the supplied array variant constructor
macro_rules! impl_string_or_array_opt_deserializer {
    (
        $fn_name:ident,
        $type_name:ident,
        $variant_arr:path,
        $str_mapper:expr
    ) => {
        pub fn $fn_name<'de, D>(deserializer: D) -> Result<Option<$type_name>, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            struct FastVisitor;

            impl<'de> serde::de::Visitor<'de> for FastVisitor {
                type Value = Option<$type_name>;

                fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                    formatter.write_str("a string, an array, or null")
                }

                fn visit_unit<E>(self) -> Result<Self::Value, E>
                where
                    E: serde::de::Error,
                {
                    Ok(None)
                }

                fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
                where
                    E: serde::de::Error,
                {
                    self.visit_string(value.to_owned())
                }

                fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
                where
                    E: serde::de::Error,
                {
                    let mapper = $str_mapper;
                    Ok(Some(mapper(value)))
                }

                fn visit_seq<A>(self, seq: A) -> Result<Self::Value, A::Error>
                where
                    A: serde::de::SeqAccess<'de>,
                {
                    let items = serde::Deserialize::deserialize(
                        serde::de::value::SeqAccessDeserializer::new(seq),
                    )?;
                    Ok(Some($variant_arr(items)))
                }
            }

            deserializer.deserialize_any(FastVisitor)
        }
    };
}
