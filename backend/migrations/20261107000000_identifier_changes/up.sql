-- Verified accounts change their email address only after the link sent to the
-- new address is opened. The pending address travels with its single-use code.
ALTER TABLE account_codes DROP CONSTRAINT account_codes_purpose_check;
ALTER TABLE account_codes
    ADD COLUMN email TEXT CHECK (email = lower(email) AND length(email) <= 254);
ALTER TABLE account_codes ADD CONSTRAINT account_codes_purpose_check
    CHECK (purpose IN ('verify','reset','email') AND ((purpose = 'email') = (email IS NOT NULL)));
