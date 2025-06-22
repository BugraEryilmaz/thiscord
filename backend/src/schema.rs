// @generated automatically by Diesel CLI.

pub mod sql_types {
    #[derive(diesel::query_builder::QueryId, diesel::sql_types::SqlType)]
    #[diesel(postgres_type(name = "channel_type"))]
    pub struct ChannelType;

    #[derive(diesel::query_builder::QueryId, diesel::sql_types::SqlType)]
    #[diesel(postgres_type(name = "permission_type"))]
    pub struct PermissionType;
}

diesel::table! {
    use diesel::sql_types::*;
    use super::sql_types::ChannelType;

    channels (id) {
        id -> Uuid,
        #[max_length = 255]
        name -> Varchar,
        #[sql_name = "type"]
        type_ -> ChannelType,
        hidden -> Bool,
        server_id -> Uuid,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    joined_users (id) {
        id -> Uuid,
        user_id -> Uuid,
        server_id -> Uuid,
    }
}

diesel::table! {
    use diesel::sql_types::*;
    use super::sql_types::PermissionType;

    permissions (id) {
        id -> Uuid,
        role_id -> Uuid,
        #[sql_name = "type"]
        type_ -> PermissionType,
    }
}

diesel::table! {
    roles (id) {
        id -> Uuid,
        #[max_length = 255]
        name -> Varchar,
        server_id -> Uuid,
    }
}

diesel::table! {
    servers (id) {
        id -> Uuid,
        #[max_length = 255]
        name -> Varchar,
        connection_string -> Text,
        image_url -> Nullable<Text>,
        created_at -> Timestamptz,
        image_path -> Nullable<Text>,
    }
}

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
    user_roles (user_id, server_id) {
        user_id -> Uuid,
        role_id -> Uuid,
        server_id -> Uuid,
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

diesel::joinable!(channels -> servers (server_id));
diesel::joinable!(joined_users -> servers (server_id));
diesel::joinable!(joined_users -> users (user_id));
diesel::joinable!(permissions -> roles (role_id));
diesel::joinable!(roles -> servers (server_id));
diesel::joinable!(user_activations -> users (user_id));
diesel::joinable!(user_roles -> roles (role_id));
diesel::joinable!(user_roles -> servers (server_id));
diesel::joinable!(user_roles -> users (user_id));

diesel::allow_tables_to_appear_in_same_query!(
    channels,
    joined_users,
    permissions,
    roles,
    servers,
    user_activations,
    user_roles,
    users,
);
