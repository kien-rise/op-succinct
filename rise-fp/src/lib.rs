pub mod common {
    pub mod args;
    pub mod contract;
    pub mod primitives;
    pub mod state;
}

pub mod components {
    pub mod game_creator;
    pub mod game_fetcher;
    pub mod tx_manager;
}

pub mod rpc {
    pub mod cl;
    pub mod el;
    pub mod utils;
}

pub mod proof {
    pub mod host;
}
