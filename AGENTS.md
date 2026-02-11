# Repository Guidelines

## Project Structure & Module Organization
Jan is a Yarn workspaces monorepo. React UI lives in `web-app/`, shared TypeScript APIs in `core/`, and feature bundles under `extensions/`. Native bindings and build assets sit in `src-tauri/` (with plugins in `src-tauri/plugins/`). Cross-cutting scripts live in `scripts/`, reusable QA helpers in `autoqa/`, and higher-level scenarios in `tests/`. Check the `docs/` directory when you need product copy or architectural narratives before editing UI flows.

## Build, Test, and Development Commands
Use `make dev` for the fastest end-to-end setup; it wires assets, builds workspaces, and starts the desktop app. When iterating only on the UI, run `yarn dev` or `yarn workspace @janhq/web-app dev` to launch Vite. Produce shipping artifacts with `yarn build` (web + Tauri) or the platform-specific `yarn build:tauri:<target>` helpers. Keep dependencies fresh by running `yarn build:extensions` after modifying any extension package.

## Coding Style & Naming Conventions
TypeScript is the default across workspaces; prefer strict typing and avoid `any`. Run `yarn lint` (ESLint 9) or package-level lint scripts before committing. Follow existing casing: `PascalCase` React components, `camelCase` hooks/utilities, and `kebab-case` directories. Tauri Rust code should respect `cargo fmt`/`cargo clippy`, and front-end styling leans on Tailwind utilities—reuse tokens before adding ad-hoc CSS.

## Testing Guidelines
`vitest` powers JavaScript unit tests; execute them via `yarn test` or `yarn test:coverage`. Component tests should pair hooks/components with `*.test.ts(x)` files colocated near the source, using Testing Library where relevant. For Rust logic, run `cargo test` within `src-tauri/`. End-to-end smoke scripts live in `autoqa/`; coordinate with maintainers before expanding that suite to keep runtime manageable.

## Commit & Pull Request Guidelines
Commits follow Conventional Commit intent (`feat:`, `fix:`, `chore:`); scope them narrowly and keep messages in the imperative. Open pull requests against the `dev` branch, include concise summaries plus user-facing impact, and link GitHub issues when applicable. Demonstrate validation by pasting the commands you ran (e.g., `yarn lint`, `yarn test`, `cargo test`) and attach UI screenshots or recordings when the change alters visible behavior.

## Security & Configuration Tips
Never commit model binaries or credentials; `.env` files stay local and should mirror the samples in `src-tauri/`. When touching download flows, confirm checksums and paths align with `pre-install/` assets, and document any new environment flags in `docs/` so desktop users can opt in safely.
