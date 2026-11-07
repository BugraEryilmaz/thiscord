-- Pending email changes are discarded; their links stop working.
DELETE FROM account_codes WHERE purpose = 'email';
ALTER TABLE account_codes DROP CONSTRAINT account_codes_purpose_check;
ALTER TABLE account_codes DROP COLUMN email;
ALTER TABLE account_codes ADD CONSTRAINT account_codes_purpose_check
    CHECK (purpose IN ('verify','reset'));
