-- Admin management: disabled/force_password_reset for users, extended server_settings

ALTER TABLE users ADD COLUMN disabled INTEGER NOT NULL DEFAULT 0;
ALTER TABLE users ADD COLUMN force_password_reset INTEGER NOT NULL DEFAULT 0;

ALTER TABLE server_settings ADD COLUMN server_name TEXT NOT NULL DEFAULT 'Accord Server';
ALTER TABLE server_settings ADD COLUMN registration_policy TEXT NOT NULL DEFAULT 'open';
ALTER TABLE server_settings ADD COLUMN max_spaces INTEGER NOT NULL DEFAULT 0;
ALTER TABLE server_settings ADD COLUMN max_members_per_space INTEGER NOT NULL DEFAULT 0;
ALTER TABLE server_settings ADD COLUMN motd TEXT;
ALTER TABLE server_settings ADD COLUMN public_listing INTEGER NOT NULL DEFAULT 0;
