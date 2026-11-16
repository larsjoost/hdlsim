---
name: Bug report
about: Something in hdlsim does not work as expected
title: ''
labels: bug
assignees: ''
---

**What happened?**


**Expected behavior?**


**Command run** (copy-paste the exact command line):

```
hdlsim ...
```

**Full error output** — paste everything hdlsim printed (stderr included). If a
diagnostic shows a code (e.g. `error[E0301]`), also run and paste
`hdlsim explain E0301`:

```
...
```

**Reproducer bundle** — from your project directory, attach the `.tar`:

```
hdlsim bundle --out hdlsim-repro.tar --top <top> [files...]   # add -L/--config as usual
```

(It packs your sources, library dirs, include dirs, config, and a `REPRO.md`
with the exact reproduce command. With it, the failure can be reproduced with
`hdlsim run --bundle hdlsim-repro.tar --top <top>`.)

**Environment** — paste `hdlsim doctor` output (first line has the version):

```
hdlsim doctor
```

- hdlsim version:
- OS:

**Additional context**

