<h1 align="center">
<img src="sans.png" />
</h1>

<div align="center">
 <strong>
   Automated photo production platform
 </strong>
</div>

<div align="center">
  <!-- Build Status -->
  <a href="https://travis-ci.org/rustasync/tide">
    <img src="https://img.shields.io/travis/Libreflip/sans.svg?style=flat-square"
      alt="Build Status" />
  </a>
</div>

The software that powers Libreflip. The two primary runtime components
are `sans-server` which initialises the RESTful user interface and
handles machine scheduling and `sans-worker` which can either be
deployed on the same machine or an external computer that handles all
image computation.

Additionally there is `sansctl` which can be used to get debug
information from a local or remote server instance.

## `sans` crates

As the entire software stack is written in Rust, the three core
library components `sans-core`, `sans-types` and `sans-processing` are
available on [crates.io](http://crates.io). You can link against them to build your
own extentions and modules that hook into sans.

## Build dependencies

In order to build `sans`, there are some external dependencies that
you need.

 - git
 - cargo
 - rust nightly
 - libimagemagick 7.0
