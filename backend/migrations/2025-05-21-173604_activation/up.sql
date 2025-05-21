-- Your SQL goes here
-- Add a new column to the 'users' table
ALTER TABLE users ADD COLUMN activated BOOLEAN NOT NULL DEFAULT FALSE;

CREATE TABLE IF NOT EXISTS user_activations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID UNIQUE NOT NULL,
    activation_code VARCHAR(255) UNIQUE NOT NULL,
    valid_until TIMESTAMP DEFAULT CURRENT_TIMESTAMP + INTERVAL '1 hour',
    FOREIGN KEY (user_id) REFERENCES users(id)
);
-- Create an index on the activation_code column for faster lookups
CREATE UNIQUE INDEX idx_activation_code ON user_activations (activation_code);