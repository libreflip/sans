PROJECT TODO
============

This is the general development roadmap. Many of these will be exported to become their own issue.

### Planning tasks

 - [x] Create basic project layout
 - [x] Figure out concurrency model between front-end and backend states
 - [x] Create a module state layout

### Implementational tasks 

 - [ ] Create simple layer binding against imagemagick
 - [ ] Create simple layer binding against camera drivers
 - [ ] Create simple layer binding against OCR
 - [ ] Create simple toolkit for common file operations
 - [ ] Create a toolkit for persistent state stores
 - [ ] Design hardware communication protocol (input required)
 - [ ] Design RESTful API for front-end

## States to manage

 - Persistent settings
   - Default export settings
   - External compute servers
 - Camera state handle
 - Web API state handle
 - Worker API state handle
 - Hardware API handle
 - 