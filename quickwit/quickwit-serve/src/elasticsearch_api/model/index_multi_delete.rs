// Copyright (C) 2024 Quickwit, Inc.
//
// Quickwit is offered under the AGPL v3.0 and as commercial software.
// For commercial licensing, contact us at hello@quickwit.io.
//
// AGPL:
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as
// published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program. If not, see <http://www.gnu.org/licenses/>.

use elasticsearch_dsl::search::SearchResponse as ElasticsearchResponse;
use elasticsearch_dsl::ErrorCause;
use hyper::StatusCode;
use serde::{Deserialize, Serialize};
use serde_with::formats::PreferMany;
use serde_with::{serde_as, OneOrMany};

use super::search_query_params::ExpandWildcards;
use super::ElasticsearchError;
use crate::simple_list::{from_simple_list, to_simple_list};

// Delete index api spec: https://www.elastic.co/guide/en/elasticsearch/reference/current/indices-delete-index.html

#[serde_with::skip_serializing_none]
#[derive(Default, Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexMultiDeleteQueryParams {
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub allow_no_indices: Option<bool>,
    #[serde(serialize_with = "to_simple_list")]
    #[serde(deserialize_with = "from_simple_list")]
    #[serde(default)]
    pub expand_wildcards: Option<Vec<ExpandWildcards>>,
}

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Default, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexMultiDeleteHeader {
    #[serde(default)]
    pub allow_no_indices: Option<bool>,
    #[serde(default)]
    pub expand_wildcards: Option<Vec<ExpandWildcards>>,
    #[serde(default)]
    pub ignore_unavailable: Option<bool>,
    #[serde_as(deserialize_as = "OneOrMany<_, PreferMany>")]
    #[serde(default)]
    pub index: Vec<String>,
    #[serde(default)]
    pub preference: Option<String>,
    #[serde(default)]
    pub request_cache: Option<bool>,
    #[serde(default)]
    pub routing: Option<Vec<String>>,
}

impl From<IndexMultiDeleteHeader> for IndexMultiDeleteQueryParams {
    fn from(header: IndexMultiDeleteHeader) -> Self {
        IndexMultiDeleteQueryParams {
            allow_no_indices: header.allow_no_indices,
            expand_wildcards: header.expand_wildcards,
            ..Default::default()
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct IndexMultiDeleteResponse {
    pub responses: Vec<IndexMultiDeleteSingleResponse>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct IndexMultiDeleteSingleResponse {
    #[serde(with = "http_serde::status_code")]
    pub status: StatusCode,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(flatten)]
    pub response: Option<ElasticsearchResponse>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorCause>,
}

impl From<ElasticsearchResponse> for IndexMultiDeleteSingleResponse {
    fn from(response: ElasticsearchResponse) -> Self {
        IndexMultiDeleteSingleResponse {
            status: StatusCode::OK,
            response: Some(response),
            error: None,
        }
    }
}

impl From<ElasticsearchError> for IndexMultiDeleteSingleResponse {
    fn from(error: ElasticsearchError) -> Self {
        IndexMultiDeleteSingleResponse {
            status: error.status,
            response: None,
            error: Some(error.error),
        }
    }
}
