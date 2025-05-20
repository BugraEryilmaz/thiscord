// @generated automatically by Diesel CLI.

diesel::table! {
    users (id) {
        id -> Uuid,
        #[max_length = 31]
        username -> Varchar,
        #[max_length = 254]
        email -> Varchar,
        password -> Text,
        deleted -> Bool,
        created_at -> Timestamp,
    }
}
