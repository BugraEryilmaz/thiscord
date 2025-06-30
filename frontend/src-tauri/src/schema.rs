// @generated automatically by Diesel CLI.

diesel::table! {
    session (id) {
        id -> Integer,
        token -> Text,
        user_id -> Text,
        username -> Text,
    }
}
