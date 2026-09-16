DROP INDEX IF EXISTS profiles_display_name_lower_idx;
ALTER TABLE profiles ADD COLUMN discriminator TEXT
    DEFAULT lpad((floor(random() * 10000))::int::text, 4, '0');
UPDATE profiles SET discriminator = lpad((floor(random() * 10000))::int::text, 4, '0')
    WHERE discriminator IS NULL;
ALTER TABLE profiles ALTER COLUMN discriminator SET NOT NULL;
CREATE UNIQUE INDEX profiles_display_name_discriminator_idx
    ON profiles (display_name, discriminator);
