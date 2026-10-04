//! Validated identifier newtypes for the NPC layer.
//!
//! All identifiers use the protocol V1 charset (`[A-Za-z0-9_.:-]`), so a
//! WorldBible entity id or a protocol `target` is always a valid NPC id. Ids are
//! validated on construction *and* on deserialization, so an invalid id can
//! never reach authoritative NPC state or a storage query.

use super::NpcError;

/// Maximum length of character, entity and location ids (same as protocol V1).
pub const MAX_ID_LEN: usize = 64;
/// Maximum length of fact ids and flag keys, which are often `prefix:entity_id`.
pub const MAX_KEY_LEN: usize = 128;

/// The entity id of the player.
pub const PLAYER_ID: &str = "player";

fn check(kind: &'static str, value: &str, max_len: usize) -> Result<(), NpcError> {
    let reason = if value.is_empty() {
        "must not be empty".to_owned()
    } else if value.len() > max_len {
        format!("must be at most {max_len} chars")
    } else if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
    {
        "may only contain [A-Za-z0-9_.:-]".to_owned()
    } else {
        return Ok(());
    };
    Err(NpcError::InvalidIdentifier { kind, reason })
}

macro_rules! id_type {
    ($(#[$doc:meta])* $name:ident, $kind:literal, $max:expr) => {
        $(#[$doc])*
        #[derive(
            Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
        )]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, NpcError> {
                let value = value.into();
                check($kind, &value, $max)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = NpcError;
            fn try_from(value: String) -> Result<Self, NpcError> {
                Self::new(value)
            }
        }

        impl std::str::FromStr for $name {
            type Err = NpcError;
            fn from_str(value: &str) -> Result<Self, NpcError> {
                Self::new(value)
            }
        }

        impl From<$name> for String {
            fn from(id: $name) -> String {
                id.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

id_type!(
    /// Id of an NPC. Matches the WorldBible character id.
    CharacterId,
    "character_id",
    MAX_ID_LEN
);
id_type!(
    /// Id of anything an NPC can relate to or remember: an NPC, the player
    /// (`"player"`), a faction, an object.
    EntityId,
    "entity_id",
    MAX_ID_LEN
);
id_type!(
    /// Id of a location. Matches the WorldBible location id.
    LocationId,
    "location_id",
    MAX_ID_LEN
);
id_type!(
    /// Id of a discrete piece of knowledge, e.g. `secret:lab_location`.
    FactId,
    "fact_id",
    MAX_KEY_LEN
);
id_type!(
    /// Key of a world flag, e.g. `mission_failed:heist`.
    FlagKey,
    "flag_key",
    MAX_KEY_LEN
);

impl EntityId {
    /// The player's entity id.
    pub fn player() -> Self {
        Self(PLAYER_ID.to_owned())
    }

    pub fn is_player(&self) -> bool {
        self.0 == PLAYER_ID
    }
}

impl From<CharacterId> for EntityId {
    fn from(id: CharacterId) -> Self {
        // Same charset and length limit, so this cannot fail.
        Self(id.0)
    }
}

impl From<&CharacterId> for EntityId {
    fn from(id: &CharacterId) -> Self {
        Self(id.0.clone())
    }
}

impl PartialEq<CharacterId> for EntityId {
    fn eq(&self, other: &CharacterId) -> bool {
        self.0 == other.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_protocol_identifiers() {
        assert_eq!(
            CharacterId::new("walter_white").unwrap().as_str(),
            "walter_white"
        );
        assert!(EntityId::new("faction:dea-1.b").is_ok());
        assert!(FactId::new(format!("died:{}", "x".repeat(64))).is_ok());
        assert!(EntityId::player().is_player());
    }

    #[test]
    fn rejects_invalid_identifiers() {
        for bad in ["", "has space", "semi;colon", "quote'", "ünïcode", "a/b"] {
            assert!(
                matches!(
                    CharacterId::new(bad),
                    Err(NpcError::InvalidIdentifier {
                        kind: "character_id",
                        ..
                    })
                ),
                "{bad:?} should be rejected"
            );
        }
        assert!(CharacterId::new("x".repeat(MAX_ID_LEN)).is_ok());
        assert!(CharacterId::new("x".repeat(MAX_ID_LEN + 1)).is_err());
        assert!(FlagKey::new("x".repeat(MAX_KEY_LEN + 1)).is_err());
    }

    #[test]
    fn deserialization_validates() {
        let ok: CharacterId = serde_json::from_str("\"hank\"").unwrap();
        assert_eq!(ok.to_string(), "hank");
        assert!(serde_json::from_str::<CharacterId>("\"hank; DROP TABLE\"").is_err());
        assert!(serde_json::from_str::<LocationId>("\"\"").is_err());
        assert_eq!(serde_json::to_string(&ok).unwrap(), "\"hank\"");
    }

    #[test]
    fn character_id_converts_to_entity_id() {
        let c = CharacterId::new("hank").unwrap();
        let e: EntityId = (&c).into();
        assert_eq!(e, c);
        assert!(!e.is_player());
    }
}
