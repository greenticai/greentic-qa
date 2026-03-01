# component-qa

A Rust + WASI-P2 Greentic component scaffolded via `greentic-component new`.

Canonical world target: `greentic:component/component@0.6.0`.
Legacy compatibility notes: `docs/vision/legacy.md` in the `greentic-component` repo.

## Requirements

- Rust 1.91+
- `wasm32-wasip2` target (`rustup target add wasm32-wasip2`)
- `cargo-component` (`cargo install cargo-component --locked`)

## Getting Started

```bash
cargo component build --release --target wasm32-wasip2
cargo test
```

The generated `component.manifest.json` references the release artifact at
`target/wasm32-wasip2/release/component_qa.wasm`.
Update the manifest hash by running `greentic-component hash component.manifest.json`.
Validate the artifact by running
`greentic-component doctor target/wasm32-wasip2/release/component_qa.wasm --manifest component.manifest.json`.

## i18n Workflow

```bash
./tools/i18n.sh
cargo build
```

- `tools/i18n.sh` reads `assets/i18n/locales.json` and generates locale JSON files from `assets/i18n/en.json`.
- `build.rs` embeds all `assets/i18n/*.json` locale dictionaries into the WASM as a CBOR bundle.

## QA Setup Workflow (Pack Assets)

1. Generate a real form with `greentic-qa new` (or `greentic-qa generate`).
2. Copy the generated form file into pack assets, for example:
   - `qa/forms/support.form.json`
3. Add locale dictionaries for that form under pack assets, for example:
   - `qa/i18n/en.json`
   - `qa/i18n/nl.json`
4. Configure `component-qa` with:

```json
{
  "qa_form_asset_path": "qa/forms/support.form.json"
}
```

`component-qa` loads the form from assets at runtime (WASI filesystem). If no
`qa_form_asset_path` is configured, setup fails fast with a guidance error.

When a form question uses `title_i18n` / `description_i18n`, `component-qa`
resolves defaults from the selected locale and falls back to `en`. On load,
all referenced i18n keys are validated against `qa/i18n/en.json`.

## QA Ops Local Test

- `qa-spec`: emit/expect setup-mode DTO semantics (`setup|update|remove`); input accepts `default|setup|install|update|upgrade|remove`.
- `apply-answers`: invoke with `{ "mode": "setup", "answers": {...}, "current_config": {...} }` (`install` accepted as alias).
- `i18n-keys`: invoke with `{}` to list keys referenced by QA/setup paths.

## Notes

- `qa_form_asset_path` is the canonical setup hook for real pack deployments.
- The placeholder fixture is kept for tests only and is no longer used as a runtime default.
