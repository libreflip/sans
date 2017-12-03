![Libreflip sans](sans.png)

Libreflip backend daemon; written in Rust.

It handles scan, processing and export jobs. It can be connected to other `sans` instances on the network as external workers. It is currently still *very* early in development and many functions will not work properly.

## Build dependencies

`sans` is distributed as a single binary with all dependencies statically linked. However, if you want to build it on your own computer there are some things that you need.

 - git
 - cargo
 - rust nightly
 - libimagemagick 7.0