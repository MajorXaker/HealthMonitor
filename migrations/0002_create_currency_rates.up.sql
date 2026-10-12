-- Latest fetched value per currency / commodity pair (v1.2).
-- One row per pair, updated in place after every successful fetch, so the
-- backend can serve the last known rates right after a restart and skip
-- re-fetching values that are still fresh.
CREATE TABLE IF NOT EXISTS currency_rates (
    pair        TEXT PRIMARY KEY,              -- e.g. 'BTC-USD', 'XAU-USD', 'RUB-BYN'
    rate_group  TEXT NOT NULL,                 -- 'crypto' | 'metal' | 'nbrb'
    value       DOUBLE PRECISION NOT NULL,     -- price of `scale` units of the base
    scale       INTEGER NOT NULL DEFAULT 1,    -- NBRB quotes RUB per 100
    source      TEXT NOT NULL,                 -- 'coinbase', 'gold-api.com', 'nbrb.by', ...
    rate_date   DATE,                          -- official rate date (NBRB only)
    fetched_at  TIMESTAMPTZ NOT NULL           -- last successful fetch
);
