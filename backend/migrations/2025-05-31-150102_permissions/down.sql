-- This file should undo anything in `up.sql`
DROP INDEX IF EXISTS idx_permissions_role_id;
DROP INDEX IF EXISTS idx_permissions_role_id_type;
DROP TABLE IF EXISTS permissions;
DROP TYPE IF EXISTS permission_type;
DROP TABLE IF EXISTS user_roles;
DROP TABLE IF EXISTS roles;