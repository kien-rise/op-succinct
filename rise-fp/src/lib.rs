pub mod common {
    pub mod primitives;
    pub mod state;
}

pub mod components {
    pub mod app_driver;
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
