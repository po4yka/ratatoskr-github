-- Run as a PostgreSQL administrator after Platform's `deploy/postgres/01-database-and-roles.sql` (which creates the shared
-- database `ratatoskr`) and before the first start of ratatoskr-github. Set the password separately with
-- `\password ratatoskr_github`; no credential belongs in this repository. The service creates and owns the
-- `github_catalog` schema inside `ratatoskr` when it starts, so the role needs `create` on the database and nothing else.

do $$
begin
    if not exists (select 1 from pg_roles where rolname = 'ratatoskr_github') then
        create role ratatoskr_github login;
    end if;
end
$$;

alter role ratatoskr_github nosuperuser nocreatedb nocreaterole noreplication nobypassrls;
grant connect, create on database ratatoskr to ratatoskr_github;

\connect ratatoskr
revoke all on schema public from ratatoskr_github;
