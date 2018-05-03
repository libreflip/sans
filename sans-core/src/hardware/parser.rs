//! Utilities for parsing incoming messages

use super::Response;

/// Parse the meaning of this craziness
pub fn get_response(incoming: Vec<u8>) -> Response {
    return Response {
        state: incoming[0],
        length: incoming[1],
    };
}

