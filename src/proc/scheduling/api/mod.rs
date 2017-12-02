//! Scheduling API submodule for sans
//! 
//! According to the sans::proc design principles, there is an inter-daemon
//! API that can be used to dispatch work to other instances of sans running
//! on the network. This means that there is a receive and transmit channel
//! for data between daemons.
//! 
//! This API module implements both the receiver and transmitter of these
//! channels. A local net worker would use the receiving end to transmit data,
//! then using receivers (and transmitters) on both sides of the scheduler to
//! exchange the compute results.
