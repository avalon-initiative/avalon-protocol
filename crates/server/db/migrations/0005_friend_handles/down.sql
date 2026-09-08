DROP INDEX IF EXISTS profiles_display_name_discriminator_idx;
ALTER TABLE profiles DROP COLUMN discriminator;
