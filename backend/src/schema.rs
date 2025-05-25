// @generated automatically by Diesel CLI.

diesel::table! {
    user_activations (id) {
        id -> Uuid,
        user_id -> Uuid,
        #[max_length = 255]
        activation_code -> Varchar,
        valid_until -> Nullable<Timestamp>,
    }
}

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
        activated -> Bool,
    }
}

diesel::joinable!(user_activations -> users (user_id));

diesel::allow_tables_to_appear_in_same_query!(user_activations, users,);
