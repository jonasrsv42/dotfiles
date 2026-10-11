# bloaty

- Read ARCHITECTURE.md first: pipeline, invariants, and recipes for the
  usual changes (which files to touch, which tests tell you).
- Run `./check.sh` before finishing (fmt, clippy -D warnings, all tests);
  `./check.sh --install` also installs the binary the Neovim plugin runs.
- Never edit tests/golden/*.json (or run `UPDATE_GOLDEN=1`) without
  explaining every line of the diff: goldens are the behaviour.
- Any JSON shape change bumps `SCHEMA` in src/main.rs and in
  lua/mbloaty/init.lua together.
- plan.md (gitignored) is the design log; append decisions there.
