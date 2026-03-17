-- Terms of Service support for server_settings

ALTER TABLE server_settings ADD COLUMN tos_enabled INTEGER NOT NULL DEFAULT 1;
ALTER TABLE server_settings ADD COLUMN tos_text TEXT;
ALTER TABLE server_settings ADD COLUMN tos_version INTEGER NOT NULL DEFAULT 1;
ALTER TABLE server_settings ADD COLUMN tos_url TEXT;

UPDATE server_settings SET tos_text = '# Terms of Service

By using this server, you agree to the following:

## Respectful Conduct
- Treat all members with respect. Harassment, bullying, hate speech, and discrimination are not tolerated.
- Do not impersonate other users, staff, or public figures.

## Content Rules
- Do not share illegal content of any kind.
- Do not post spam, unsolicited advertising, or phishing links.
- NSFW content is only permitted in channels explicitly marked as NSFW.
- Do not share others'' private information (doxxing).

## Moderation
- Moderators may delete messages, timeout, kick, or ban members who violate these terms.
- Moderation actions are logged for accountability.
- If you believe an action was taken in error, contact a server administrator.

## Privacy
- This server may log messages and activity for moderation purposes.
- Your data is handled according to the server operator''s privacy practices.

## Changes
- These terms may be updated at any time. Continued use after changes constitutes acceptance.' WHERE id = 1;
