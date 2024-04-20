-- Add migration script here
ALTER TABLE `users` ADD COLUMN `google_calendar_id` TEXT;
ALTER TABLE `users` ADD COLUMN `google_calendar_acl_id` TEXT;
DROP TABLE `server_events`;
CREATE TABLE `server_events` (
    `discord_id` INTEGER(64) NOT NULL,
    `user_id` INTEGER(64) NOT NULL,
    `google_event_id` TEXT NOT NULL,
    PRIMARY KEY (`google_event_id`, `user_id`),
    UNIQUE (`user_id`, `discord_id`)
);
