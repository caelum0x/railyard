-- Scribase bridge ordering (Wave P P-Y1). Idempotent: every statement can run
-- again without error or change. Not applied by agents to any hosted
-- database; the owner applies it after 0003 (owner step), agents only to a
-- scratch Postgres.
--
-- Each orgs sync from Scribase carries addon.as_of, the Scribase time (epoch
-- milliseconds) of the add-on state it mirrors. The bridge stores it here and
-- refuses to apply an older state, so a late or out-of-order delivery inside
-- the signature tolerance can never flip an entitlement back.

alter table railyard.entitlements add column if not exists addon_as_of bigint;

do $$
begin
  if not exists (
    select 1 from pg_constraint
    where conname = 'entitlements_addon_as_of_check'
      and conrelid = 'railyard.entitlements'::regclass
  ) then
    alter table railyard.entitlements
      add constraint entitlements_addon_as_of_check check (addon_as_of is null or addon_as_of > 0);
  end if;
end $$;
