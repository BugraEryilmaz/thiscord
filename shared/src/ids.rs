use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! uuid_id {
    ($name:ident) => {
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            pub const fn from_uuid(value: Uuid) -> Self {
                Self(value)
            }

            pub const fn as_uuid(self) -> Uuid {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                value.parse().map(Self)
            }
        }
    };
}

// Generation lives on the backend; shared/WASM requires no random-number source.
uuid_id!(InstanceId);
uuid_id!(RequestId);
uuid_id!(AccountId);
uuid_id!(SessionId);
uuid_id!(GuildId);
uuid_id!(RoleId);
uuid_id!(ChannelId);
