//! Byte fields cross the wire and the database in two different shapes.
//!
//! Go's `[]byte` marshals to a base64 **string** in JSON and to a BSON
//! **binary** value in Mongo. Rust's `Vec<u8>` does neither by default: serde
//! renders it as an array of integers in both. Getting this wrong is silent --
//! the data round-trips within one implementation and is unreadable to the
//! other -- so the two representations are separated explicitly here.
//!
//! Use [`base64_bytes`] on JSON DTOs and `serde_bytes` on stored documents.

/// serde helper for `Vec<u8>` fields that must appear as base64 in JSON.
pub mod base64_bytes {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let encoded = String::deserialize(deserializer)?;
        STANDARD
            .decode(encoded.as_bytes())
            .map_err(serde::de::Error::custom)
    }
}

/// Same, for `Option<Vec<u8>>`.
///
/// Needed wherever Go writes a JSON `null` for an absent value -- notably the
/// verification key, where "no key" and "empty key" are distinct states.
pub mod base64_bytes_opt {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        bytes: &Option<Vec<u8>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match bytes {
            Some(bytes) => serializer.serialize_str(&STANDARD.encode(bytes)),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Vec<u8>>, D::Error> {
        let encoded = Option::<String>::deserialize(deserializer)?;
        encoded
            .map(|value| STANDARD.decode(value.as_bytes()))
            .transpose()
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Payload {
        #[serde(with = "super::base64_bytes")]
        data: Vec<u8>,
        #[serde(with = "super::base64_bytes_opt")]
        key: Option<Vec<u8>>,
    }

    #[test]
    fn renders_bytes_as_base64_strings_not_integer_arrays() {
        let json = serde_json::to_string(&Payload {
            data: vec![0, 1, 255],
            key: Some(b"k".to_vec()),
        })
        .unwrap();

        assert_eq!(json, r#"{"data":"AAH/","key":"aw=="}"#);
    }

    #[test]
    fn absent_and_empty_are_distinguishable() {
        let absent = serde_json::to_string(&Payload {
            data: vec![],
            key: None,
        })
        .unwrap();
        assert_eq!(absent, r#"{"data":"","key":null}"#);
    }

    #[test]
    fn round_trips_arbitrary_bytes() {
        let original = Payload {
            data: (0u8..=255).collect(),
            key: None,
        };
        let json = serde_json::to_string(&original).unwrap();
        assert_eq!(serde_json::from_str::<Payload>(&json).unwrap(), original);
    }
}
