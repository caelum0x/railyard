-- Mint self-serve account provisioning (backlog item 11): each console app
-- gets its own billing ledger account on Mint (apps/mint), created on demand
-- from the Apps tab instead of an operator curling `POST /v1/accounts` by
-- hand. Nullable: apps created before this ship, or while MINT_URL/
-- MINT_API_KEY are unset, simply have no account yet.

alter table railyard.apps
  add column if not exists mint_account_id text;

create index if not exists apps_mint_account_idx on railyard.apps (mint_account_id) where mint_account_id is not null;
