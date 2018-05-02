![Libreflip sans](sans.png)

[![](https://travis-ci.org/libreflip/sans.svg?branch=master)](https://travis-ci.org/libreflip/sans)
[![](https://coveralls.io/repos/github/libreflip/sans/badge.svg?branch=master&service=github)](https://coveralls.io/github/libreflip/sans?branch=master)

<!--
[![](https://docs.rs/barrel/badge.svg)](https://docs.rs/barrel/) 
[![](https://img.shields.io/crates/v/barrel.svg)](https://crates.io/crates/barrel)
[![](https://img.shields.io/crates/d/barrel.svg)](https://crates.io/crates/barrel)
-->

The software that powers Libreflip. The two primary runtime components are `sans-server` which initialises the RESTful user interface and handles machine scheduling and `sans-worker` which can either be deployed on the same machine or an external computer that handles all image computation.

Additionally there is `sansctl` which can be used to get debug information from a local or remote server instance.

## `sans` crates

As the entire software stack is written in Rust, the three core library components `sans-core`, `sans-types` and `sans-processing` are available on [crates.io](). You can link against them to build your own extentions and modules that hook into sans.

## Build dependencies

In order to build `sans`, there are some external dependencies that you need.

 - git
 - cargo
 - rust nightly
 - libimagemagick 7.0