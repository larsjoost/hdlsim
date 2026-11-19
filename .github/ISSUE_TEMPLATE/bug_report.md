---
name: Bug report
about: Something in hdlsim does not work as expected
title: ''
labels: bug
assignees: ''
---

> **Read this before pasting anything.** Issues are **public**, and so is every
> attachment and gist link — **never paste proprietary or confidential code.**
>
> **What makes an issue fixable** (the goal: reproducible without follow-up
> questions):
>
> 1. The exact command you ran and the **full error output** (stderr included);
>    if a diagnostic shows a code (e.g. `error[E0301]`), also run and paste
>    `hdlsim explain E0301`.
> 2. The `hdlsim doctor` output (first line has the version) and your OS.
> 3. A **minimal reproducer bundle** built from the bundled examples — see below.

**Minimal reproducer (privacy-safe)** — reduce the failure to *new* code based
on a bundled example instead of your own project:

```
cp -r examples/vhdl-counter repro     # or examples/counter (Verilog),
                                      # examples/features, examples/fp-vhdl, ...
cd repro
# modify the copied files until hdlsim fails on them
hdlsim run --top tb_counter counter.vhd tb_counter.vhd
hdlsim bundle --out hdlsim-repro.tar --top tb_counter counter.vhd tb_counter.vhd
```

Attach only that `hdlsim-repro.tar`. Anyone can then reproduce with
`hdlsim run --bundle hdlsim-repro.tar --top tb_counter`. If the bug truly needs
your real design, file anyway with the output and `doctor` below and say so —
we will suggest a safe reduction.

(If you filed via `hdlsim ... --report "..."`, the bundle was uploaded as a gist
and linked in the issue body. The gist is unlisted but reachable through this
public issue — remove the link if it contains code you cannot share.)

**What happened?**


**Expected behavior?**


**Command run** (copy-paste the exact command line):

```
hdlsim ...
```

**Full error output** — paste everything hdlsim printed (stderr included):

```
...
```

**Reproducer bundle** — attach the `.tar` packed from the *reduced* example:

```
hdlsim bundle --out hdlsim-repro.tar --top <top> [files...]   # add -L/--config as usual
```

(It packs the sources, library dirs, include dirs, config, and a `REPRO.md`
with the exact reproduce command. Reproduce from it with
`hdlsim run --bundle hdlsim-repro.tar --top <top>`.)

**Environment** — paste `hdlsim doctor` output (first line has the version):

```
hdlsim doctor
```

- hdlsim version:
- OS:

**Additional context**

