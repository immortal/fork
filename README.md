# fork

[![crates.io](https://img.shields.io/crates/v/fork.svg)](https://crates.io/crates/fork)
[![Build Status](https://travis-ci.org/immortal/fork.svg?branch=master)](https://travis-ci.org/immortal/fork)

Library for creating a new process detached from the controling terminal
(daemon) using the [fork](https://www.freebsd.org/cgi/man.cgi?fork) and
[setsid](https://www.freebsd.org/cgi/man.cgi?setsid) syscalls.

## Why?

- [daemon(3)](http://man7.org/linux/man-pages/man3/daemon.3.html) has been
deprecated in MacOSX 10.5, by using `fork` and `setsid` new methods could be
created to achieve the same goal, inspired by ["nix - Rust friendly bindings to
*nix APIs crate"](https://crates.io/crates/nix).
- Minimal library to daemonize, fork, double-fork a process
- Learn Rust :crab:
