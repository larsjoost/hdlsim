# Local HDL projects (gitignored)

This folder holds **private HDL designs** used to exercise hdlsim during
development. Everything here stays on this machine and out of the repository.

**One subfolder per design** — each is its own standalone project:

```
project/
  design-a/     <- its own project, its own hdlsim.ini
  design-b/
```

## Set up a design

`hdlsim init` runs **inside the individual design folder**, not here:

```bash
cd project/design-a
cargo run -p hdlsim -- init
# then edit hdlsim.ini to map libraries to subdirectories, if needed
```

## Run a simulation

Also from **inside the design folder**, so its parse cache and wave output
land there:

```bash
cd project/design-a
cargo run -p hdlsim -- run tb_top --wave wave.html --vcd wave.vcd
```

(`cargo run -p hdlsim` works from subdirectories — it finds the workspace
root automatically. `.hdlsim-cache/` and `*.vcd` are gitignored at any depth.)

## Privacy rule

Nothing from these designs may be traceable in the hdlsim repository — see
`docs/project-context.md` (Security constraints) for the exact policy.
