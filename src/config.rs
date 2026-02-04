/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Theme {
    /// Where to fetch the theme from.
    pub repository: Url,

    /// Specific revision to checkout from the theme repository.
    pub commit: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Location {
    /// Git repository to fetch proposals from.
    pub repository: Url,

    /// Location where the rendered HTML and assets will end up.
    pub base_url: Url,

    /// A commit hash that exists solely in this repository.
    ///
    /// Use to determine which repository is being rendered. Pick a commit after every other
    /// location/working group/etc. split off.
    pub identifying_commit: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Locations(pub HashMap<String, Location>);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub theme: Theme,
    pub locations: Locations,
}

impl Config {
    pub fn production() -> Self {
        let mut locations = HashMap::new();

        locations.insert(
            "EIPs".into(),
            Location {
                repository: "https://github.com/ethereum/EIPs.git".try_into().unwrap(),
                base_url: "https://eips.ethereum.org/".try_into().unwrap(),
                identifying_commit: "0f44e2b94df4e504bb7b912f56ebd712db2ad396".into(),
            },
        );

        locations.insert(
            "ERCs".into(),
            Location {
                repository: "https://github.com/ethereum/ERCs.git".try_into().unwrap(),
                base_url: "https://ercs.ethereum.org/".try_into().unwrap(),
                identifying_commit: "8dd085d159cb123f545c272c0d871a5339550e79".into(),
            },
        );

        Self {
            theme: Theme {
                repository: "https://github.com/ethereum/eips-theme.git"
                    .try_into()
                    .unwrap(),
                commit: "88a58af77795e92388bbfbe913650644257dd2d4".into(),
            },
            locations: Locations(locations),
        }
    }

    pub fn staging() -> Self {
        let mut locations = HashMap::new();

        locations.insert(
            "EIPs".into(),
            Location {
                repository: "https://github.com/eips-wg/EIPs.git".try_into().unwrap(),
                base_url: "https://eips-wg.github.io/EIPs/".try_into().unwrap(),
                identifying_commit: "0f44e2b94df4e504bb7b912f56ebd712db2ad396".into(),
            },
        );

        locations.insert(
            "ERCs".into(),
            Location {
                repository: "https://github.com/eips-wg/ERCs.git".try_into().unwrap(),
                base_url: "https://eips-wg.github.io/ERCs/".try_into().unwrap(),
                identifying_commit: "8dd085d159cb123f545c272c0d871a5339550e79".into(),
            },
        );

        Self {
            theme: Theme {
                repository: "https://github.com/eips-wg/theme.git".try_into().unwrap(),
                commit: "88a58af77795e92388bbfbe913650644257dd2d4".into(),
            },
            locations: Locations(locations),
        }
    }
}
