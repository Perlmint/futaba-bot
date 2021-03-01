INSERT INTO history (message_id, user_id, date) VALUES ($1, $2, $3);
UPDATE users SET count = count + 1 WHERE user_id = $2;