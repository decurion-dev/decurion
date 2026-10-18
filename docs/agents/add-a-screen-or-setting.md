# Add a screen, a setting or an API route

Template to copy: the per-app **websites** setting. It touched every layer
once (commit `793f21c`). Read those pieces first.

| Layer | Websites example |
| --- | --- |
| Migration | `crates/decurion-hub/migrations/0043_sites.sql` |
| Shared type | `App.sites`, `NewApp.sites` in `crates/decurion-proto/src/lib.rs`; rules in `src/sites.rs` |
| Store | `APP_COLS`, `app_from`, `add_app`, `set_sites`, `clean_sites` in `crates/decurion-hub/src/store.rs` |
| Route + handler | `.route("/apps/{id}/sites", post(set_sites))` and `async fn set_sites` in `crates/decurion-hub/src/lib.rs` |
| Screen | `sitesBlock`, `saveSites`, `loadBuiltIn` in `crates/decurion-hub/src/board.html` |
| Tests | `an_apps_websites_are_cleaned_up_when_added`, `a_bad_website_is_refused_by_name`, `only_the_owner_chooses_an_apps_websites`, `the_owner_changes_an_apps_websites_and_the_next_run_gets_them` in `crates/decurion-hub/tests/api.rs` |
| Owner help | "Choose which websites agents may use" in `docs/help/src/add-your-first-app.md` |

## A new field on an app (or thread)

1. **Migration.** New file with the next free number above the highest in `crates/decurion-hub/migrations/` (today `0045`, so `0046_<name>.sql`). Begin with a `--` comment saying why. Use `ALTER TABLE ... ADD COLUMN ... NOT NULL DEFAULT ...` so old rows stay valid; add a `CHECK` for closed sets (see `0041_accounts.sql`). Never edit a shipped migration.
2. **Type.** Add the field to the struct in `decurion-proto` with a doc comment and `#[serde(default)]`, so old clients and stored JSON still parse.
3. **Store.** Add the column to `APP_COLS` (or `THREAD_COLS`) and to `app_from` / `thread_from`. Validate in the store, not only in the page; return `StoreError::Invalid("plain words the owner can act on")` (HTTP 400). `Refused` and `Duplicate` become 409, `NotFound` 404 (`impl IntoResponse for ApiError`).
4. **Route.** In `router()` add it to the `api` router (everything there needs a key, via `require_key`). In the handler, take `axum::Extension(c): axum::Extension<Caller>` and start with `if c != Caller::Owner { return forbidden("doing X"); }` unless the runner needs it. The runner key can call any `/api` route whose handler does not check; cloud workers can call only what `cloud_reach` lists, so do not add to that list unless a running box truly needs it.
5. **Taps that change things** should take a `receipt` like `add_thread` and `request_move` (`check_receipt`, `receipt_result`, `save_receipt`), so a resend does nothing twice.
6. **Does the box see it?** If the field changes how a box is built or what it can reach (like `sites`), it travels in the `Claim`, the runner applies it, and you bump `RULES_VERSION` in `crates/decurion-proto/src/lib.rs`.

## The screen

- Pages are single files in `crates/decurion-hub/src/` (`board.html`, `inbox.html`, `goals.html`, `cloud.html`). Each has its own `<style>` and `<script>` and loads `/ui.css` and `/ui.js`.
- Call the API with `D.api("/apps/1/sites", { method: "POST", body: JSON.stringify(...) })` (it adds the `/api` prefix itself); it throws with `e.message` set to the hub's plain error text and `e.signedOut` when signed out. Show errors next to the field (`fieldErr`) or with `D.toast(text, "alert")`.
- Build elements with `D.el(tag, attrs, ...children)`. Never set `innerHTML` from data: names come from the owner and agent text comes from agents.
- The CSP (`CSP` in `lib.rs`) allows only same-origin scripts and styles. No CDNs, no outside fonts.
- Words: say what the owner gets, not how it works ("stripe.com can now be used. This applies to its next piece of work.").

## A new page

1. Add `crates/decurion-hub/src/<name>.html`.
2. In `lib.rs`: `async fn <name>_page() -> Response { page(include_str!("<name>.html")) }` and `.route("/<name>", get(<name>_page))` on the outer router (not under `/api`). `page()` adds the CSP and no-store headers.
3. Link to it from the board's sidebar or Settings.
4. Test it is served with the board's rules, like `board_sends_a_content_security_policy` (`tests/api.rs`) or `the_cloud_page_is_served_with_the_board_rules` (`tests/places.rs`).

## Tests

API tests drive the real router in memory: `hub()` gives `(Router, Store)` on `sqlite::memory:`, `call(&r, "POST", path, Some(OWNER), json!(...))` returns `(StatusCode, Value)`, `seed(&r)` adds an app and a thread. Write at least: the happy path, a bad value refused with plain text, and `Some(RUNNER)` getting `FORBIDDEN`.

```sh
cargo test -p decurion-hub --test api only_the_owner_chooses_an_apps_websites
```

Then the full checks in `AGENTS.md`. If the owner will notice, update the help page and `CHANGELOG.md`.
