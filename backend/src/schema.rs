// @generated automatically by Diesel CLI.

diesel::table! {
    instance (singleton) {
        singleton -> Bool,
        id -> Uuid,
        created_at -> Timestamptz,
    }
}
