---
title: Aggregations API
sidebar_position: 30
---

An aggregation summarizes your data as statistics on buckets or metrics.

Aggregations can provide answer to questions like:

- What is the average price of all sold articles?
- How many errors with status code 500 do we have per day?
- What is the average listing price of cars grouped by color?

There are two categories: [Metrics](#metric-aggregations) and [Buckets](#bucket-aggregations).

#### Prerequisite

To be able to use aggregations on a field, the field needs to have a fast field index created. A fast field index is a columnar storage,
where documents values are extracted and stored to.

Example to create a fast field on text for term aggregations.
```yaml
name: category
type: text
tokenizer: raw
record: basic
fast: true
```

See the [index configuration](../configuration/index-config.md) page for more details and examples.

#### API Endpoint

The endpoints for aggregations are the search endpoints:
- Quickwit API: `api/v1/<index id>/search`
- Elasticsearch API: `api/v1/_elastic/<index_id>/_search`.

#### Format

The aggregation request and result de/serialize into elasticsearch compatible JSON.
If not documented otherwise you should be able to drop in your elasticsearch aggregation queries.

In some examples below is not the full request shown, but only the payload for `aggregations`.

#### Example

Request
```json skip
{
    "query": "*",
    "max_hits": 0,
    "aggs": {
        "sites_and_aqi": {
            "terms": {
                "field": "County",
                "size": 2,
                "order": { "average_aqi": "asc" }
            },
            "aggs": {
                "average_aqi": {
                    "avg": { "field": "AQI" }
                }
            }
        }
    }
}
```


Response
```json
...
"aggregations": {
    "sites_and_aqi": {
      "buckets": [
        {
          "average_aqi": {
            "value": 32.62267569707098
          },
          "doc_count": 56845,
          "key": "臺東縣"
        },
        {
          "average_aqi": {
            "value": 35.97893635571055
          },
          "doc_count": 28675,
          "key": "花蓮縣"
        }
      ],
      "sum_other_doc_count": 1872055
    }
}
```

### Supported Aggregations

 - Bucket
    - [Histogram](#histogram)
    - [DateHistogram](#date-histogram)
    - [Range](#range)
    - [Terms](#terms)
- Metric
    - [Average](#average)
    - [Count](#count)
    - [Max](#max)
    - [Min](#min)
    - [Stats](#stats)
    - [Sum](#sum)
    - [Percentiles](#percentiles)


## Bucket Aggregations

BucketAggregations create buckets of documents. Each bucket is associated with a rule which determines whether or not a document falls into it.
In other words, the buckets effectively define document sets. Buckets are not necessarily disjunct, therefore a document can fall into multiple buckets.
In addition to the buckets themselves, the bucket aggregations also compute and return the number of documents for each bucket.
Bucket aggregations, as opposed to metric aggregations, can hold sub-aggregations.
These sub-aggregations will be aggregated for the buckets created by their “parent” bucket aggregation.
There are different bucket aggregators, each with a different “bucketing” strategy.
Some define a single bucket, some define a fixed number of multiple buckets, and others dynamically create the buckets during the aggregation process.

Example request, histogram with stats in each bucket:

#### Aggregating on datetime fields

See [`DateHistogram`](#date-histogram) for more convenient API for `datetime` fields.

Fields of type `datetime` are handled the same way as any numeric field. However, all values in the requests such as intervals, offsets, bounds, and range boundaries need to be expressed in milliseconds.

Histogram with one bucket per day on a `datetime` field. `interval` needs to be provided in milliseconds.
In the following example, we grouped documents per day (`1 day = 86400000 milliseconds`).
The returned format is currently fixed at `Rfc3339`.

##### Request
```json skip
{
  "query": "*",
  "max_hits": 0,
  "aggs": {
    "datetime_histogram":{
      "histogram":{
        "field": "datetime",
        "interval": 86400000
      }
    }
  }
}
```
##### Response

```json skip
{
  ...
  "aggregations": {
    "datetime_histogram": {
      "buckets": [
        {
          "doc_count": 1,
          "key": 1546300800000000.0,
          "key_as_string": "2019-01-01T00:00:00Z"
        },
        {
          "doc_count": 2,
          "key": 1546560000000000.0,
          "key_as_string": "2019-01-04T00:00:00Z"
        }
      ]
    }
  }
}
```

### Histogram

Histogram is a bucket aggregation, where buckets are created dynamically for the given interval. Each document value is rounded down to its bucket.

E.g. if we have a price 18 and an interval of 5, the document will fall into the bucket with the key 15. The formula used for this is: ((val - offset) / interval).floor() * interval + offset.

#### Returned Buckets

By default buckets are returned between the min and max value of the documents, including empty buckets. Setting min_doc_count to != 0 will filter empty buckets.

The value range of the buckets can bet extended via extended_bounds or limit the range via hard_bounds.

#### Example

```json
{
    "query": "*",
    "max_hits": 0,
    "aggs": {
        "prices": {
            "histogram": {
                "field": "price",
                "interval": 10
            }
        }
    }
}
```

#### Parameters

###### **field**

The field to aggregate on.

Currently this aggregation only works on fast fields of type `u64`, `f64`, `i64`, and `datetime`.

###### **keyed**

Change response format from an array to a hashmap, `key` in the bucket will be the `key` in the hashmap.

###### **interval**

The interval to chunk your data range. Each bucket spans a value range of [0..interval). Must be larger than 0.

###### **offset**

Intervals implicitly defines an absolute grid of buckets `[interval * k, interval * (k + 1))`.
Offset makes it possible to shift this grid into `[offset + interval * k, offset + interval (k + 1))`. Offset has to be in the range [0, interval).

As an example, if there are two documents with value 8 and 12 and interval 10.0, they would fall into the buckets with the key 0 and 10. With offset 5 and interval 10, they would both fall into the bucket with they key 5 and the range [5..15)

```json
{
    "query": "*",
    "max_hits": 0,
    "aggs": {
        "prices": {
            "histogram": {
                "field": "price",
                "interval": 10,
                "offset": 2.5
            }
        }
    }
}
```


###### **min_doc_count**

The minimum number of documents in a bucket to be returned. Defaults to 0.

###### **hard_bounds**

Limits the data range to [min, max] closed interval.
This can be used to filter values if they are not in the data range.
hard_bounds only limits the buckets, to force a range set both `extended_bounds` and `hard_bounds` to the same range.

```json
{
    "query": "*",
    "max_hits": 0,
    "aggs": {
        "prices": {
            "histogram": {
                "field": "price",
                "interval": 10,
                "hard_bounds": {
                    "min": 0,
                    "max": 100
                }
            }
        }
    }
}
```

###### **extended_bounds**

Can be set to extend your bounds. The range of the buckets is by default defined by the data range of the values of the documents. As the name suggests, this can only be used to extend the value range. If the bounds for min or max are not extending the range, the value has no effect on the returned buckets.
Cannot be set in conjunction with `min_doc_count` > 0, since the empty buckets from extended bounds would not be returned.

```json
{
    "query": "*",
    "max_hits": 0,
    "aggs": {
        "prices": {
            "histogram": {
                "field": "price",
                "interval": 10,
                "extended_bounds": {
                    "min": 0,
                    "max": 100
                }
            }
        }
    }
}
```

### Date Histogram

`DateHistogram` is similar to `Histogram`, but it can only be used with [datetime type](../configuration/index-config#datetime-type) and provides a more convenient API to define intervals.

Like the histogram, values are rounded down into the closest bucket.

The returned format is currently fixed at `Rfc3339`.

##### Limitations
Only fixed time intervals via the `fixed_interval` parameter are supported.
The parameters `interval` and `calendar_interval` are unsupported.

##### Request
```json skip
{
    "query": "*",
    "max_hits": 0,
    "aggs": {
        "sales_over_time": {
            "date_histogram": {
                "field": "sold_at",
                "fixed_interval": "30d"
                "offset": "-4d"
            }
        }
    }
}
```
##### Response

```json skip
{
    ...
    "aggregations": {
        "sales_over_time" : {
            "buckets" : [{
                "key_as_string" : "2015-01-01T00:00:00Z",
                "key" : 1420070400000,
                "doc_count" : 4
            }]
        }
    }
}
```


#### Parameters

###### **field**

The field to aggregate on.

Currently this aggregation only works on fast fields of type `datetime`.

###### **keyed**

Change response format from an array to a hashmap, `key` in the bucket will be the `key` in the hashmap.

###### **interval**

The interval to chunk your data range. Each bucket spans a value range of [0..interval). Must be larger than 0.

Fixed intervals are configured with the `fixed_interval` parameter.
Fixed intervals are a fixed number of SI units and
never deviate, regardless of where they fall on the calendar. One second is always
composed of 1000ms. This allows fixed intervals to be specified in any multiple of the
supported units. However, it means fixed intervals cannot express other units such as
months, since the duration of a month is not a fixed quantity. Attempting to specify a
calendar interval like month or quarter will return an Error.

The accepted units for fixed intervals are:
* `ms`: milliseconds
* `s`: seconds. Defined as 1000 milliseconds each.
* `m`: minutes. Defined as 60 seconds each (60_000 milliseconds).
* `h`: hours. Defined as 60 minutes each (3_600_000 milliseconds).
* `d`: days. Defined as 24 hours (86_400_000 milliseconds).

Fractional time values are not supported, but this can be addressed by shifting to another
time unit (e.g., `1.5h` could instead be specified as `90m`).

###### **offset**

Intervals implicitly defines an absolute grid of buckets `[interval * k, interval * (k + 1))`.
Offset makes it possible to shift this grid into `[offset + interval * k, offset + interval (k + 1))`. Offset has to be in the range [0, interval).

This is especially useful when using `fixed_interval`, to shift the first bucket e.g. at the start of the year.

The `offset` parameter is has the same syntax as the `fixed_interval` parameter, but also allows for negative values.

###### **min_doc_count**

The minimum number of documents in a bucket to be returned. Defaults to 0.

###### **hard_bounds**
Same as in [`Histogram`](#hard_bounds) but `min` and `max` parameters need to be set as timestamp with milliseconds precision.

###### **extended_bounds**
Same as in [`Histogram`](#extended_bounds) but `min` and `max` parameters need to be set as timestamp with milliseconds precision.

### Range

Provide user-defined buckets to aggregate on. Two special buckets will automatically be created to cover the whole range of values.
The provided buckets have to be continuous. During the aggregation, the values extracted from the fast_field field will be checked against each bucket range.
Note that this aggregation includes the from value and excludes the to value for each range.

#### Limitations/Compatibility

Overlapping ranges are not yet supported.

##### Request
```json skip
{
    "query": "*",
    "max_hits": 0,
    "aggs": {
        "my_scores": {
            "range": {
                "field": "score",
                "ranges": [
                    { "to": 3.0, "key": "low" },
                    { "from": 3.0, "to": 7.0, "key": "medium-low" },
                    { "from": 7.0, "to": 20.0, "key": "medium-high" },
                    { "from": 20.0, "key": "high" }
                ]
            }
        }
    }
}
```

##### Response

```json skip
{
    ...
    "aggregations": {
        "my_scores" : {
            "buckets": [
                {"key": "low", "doc_count": 0, "to": 3.0},
                {"key": "medium-low", "doc_count": 10, "from": 3.0, "to": 7.0},
                {"key": "medium-high", "doc_count": 10, "from": 7.0, "to": 20.0},
                {"key": "high", "doc_count": 80, "from": 20.0}
            ]
        }
    }
}
```

#### Parameters

###### **keyed**

Change response format from an array to a hashmap, the serialized range will be the `key` in the hashmap.
If a custom `key` is provided, it will be used instead.

###### **field**

The field to aggregate on.

Currently this aggregation only works on fast fields of type `u64`, `f64`, `i64`, and `datetime`.

###### **ranges**

The list of buckets, with `from` and `to` values.
The `from` value is inclusive in the range.
The `to` value is not inclusive in the range.
`key` is optional, and will be used as the bucket key in the response.

The first bucket can omit the `from` value, and the last bucket the `to` value.
Note that this aggregation includes the `from` value and excludes the `to` value for each range. Extra buckets will be created until the first `to`, and last `from`, if necessary.

### Terms

Creates a bucket for every unique term and counts the number of occurrences.

Request
```json skip
{
    "query": "*",
    "max_hits": 0,
    "aggs": {
        "genres": {
            "terms": { "field": "genre" }
        }
    }
}
```

Response
```json
...
"aggregations": {
    "genres": {
        "doc_count_error_upper_bound": 0,
        "sum_other_doc_count": 0,
        "buckets": [
            { "key": "drumnbass", "doc_count": 6 },
            { "key": "raggae", "doc_count": 4 },
            { "key": "jazz", "doc_count": 2 }
        ]
    }
}
```


#### Document count error
In Quickwit, we have one segment per split. Therefore the results returned from a split, is equivalent to results returned from a segment.
To improve performance, results from one split are cut off at `split_size`.
When combining results of multiple splits, terms that
don't make it in the top n of a result from a split increase the theoretical upper bound error by lowest
term-count.

Even with a larger `split_size` value, doc_count values for a terms aggregation may be
approximate. As a result, any sub-aggregations on the terms aggregation may also be approximate.
`sum_other_doc_count` is the number of documents that didn’t make it into the the top size
terms. If this is greater than 0, you can be sure that the terms agg had to throw away some
buckets, either because they didn’t fit into `size` on the root node or they didn’t fit into
`split_size` on the leaf node.

#### Per bucket document count error
If you set the `show_term_doc_count_error` parameter to true, the terms aggregation will include
doc_count_error_upper_bound, which is an upper bound to the error on the doc_count returned by
each split. It’s the sum of the size of the largest bucket on each split that didn’t fit
into `split_size`.

#### Parameters

###### **field**

The field to aggregate on.

Currently term aggregation only works on fast fields of type `text`, `f64`, `i64` and `u64`.

###### **size**

By default, the top 10 terms with the most documents are returned. Larger values for size are more expensive.

###### **split_size**

The get more accurate results, we fetch more than size from each segment/split.
Increasing this value is will increase the accuracy, but also the CPU/memory usage.

Defaults to size * 1.5 + 10.

###### **show_term_doc_count_error**

If you set the show_term_doc_count_error parameter to true, the terms aggregation will include doc_count_error_upper_bound, which is an upper bound to the error on the doc_count returned by each split.
It’s the sum of the size of the largest bucket on each split that didn’t fit into split_size.

Defaults to true when ordering by count desc.


###### **min_doc_count**

Filter all terms that are lower than `min_doc_count`. Defaults to 1.

_Expensive_ : When set to 0, this will return all terms in the field.


###### **order**

Set the order. String is here a target, which is either “_count”, “_key”, or the name of a metric sub_aggregation.
Single value metrics like average can be addressed by its name. Multi value metrics like stats are required to address their field by name e.g. “stats.avg”.


Order alphabetically
```json skip
{
    "query": "*",
    "max_hits": 0,
    "aggs": {
        "genres": {
            "terms": {
                "field": "genre",
                "order": { "_key": "asc" }
            }
        }
    }
}
```


Order by sub_aggregation

```json skip
{
    "query": "*",
    "max_hits": 0,
    "aggs": {
        "articles_by_price": {
            "terms": {
                "field": "article_name",
                "order": { "average_price": "asc" }
            },
            "aggs": {
                "average_price": {
                    "avg": { "field": "price" }
                }
            }
        }
    }
}
```



## Metric Aggregations

The aggregations in this family compute metrics based on values extracted from the documents that are being aggregated.
Values are extracted from the fast field of the document. Some aggregations output a single numeric metric (e.g. Average)
and are called single-value numeric metrics aggregation, others generate multiple metrics (e.g. Stats) and are called multi-value numeric metrics aggregation.

In contrast to bucket aggregations, metrics don't allow sub-aggregations, since there is no document set to aggregate on.

### Average

A single-value metric aggregation that computes the average of numeric values that are extracted from the aggregated documents.
Supported field types are `u64`, `f64`, `i64`, and `datetime`.

**Request**
```json skip
{
    "query": "*",
    "max_hits": 0,
    "aggs": {
        "average_price": {
            "avg": { "field": "price" }
        }
    }
}
```

**Response**
```json
{
    "num_hits": 9582098,
    "hits": [],
    "elapsed_time_micros": 101942,
    "errors": [],
    "aggregations": {
        "average_price": {
            "value": 133.7
        }
    }
}
```

### Count

A single-value metric aggregation that counts the number of values that are extracted from the aggregated documents.
Supported field types are `u64`, `f64`, `i64`, and `datetime`.

**Request**
```json skip
{
    "query": "*",
    "max_hits": 0,
    "aggs": {
        "price_count": {
            "value_count": { "field": "price" }
        }
    }
}
```

**Response**
```json
{
    "num_hits": 9582098,
    "hits": [],
    "elapsed_time_micros": 102956,
    "errors": [],
    "aggregations": {
        "price_count": {
            "value": 9582098
        }
    }
}
```

### Max

A single-value metric aggregation that computes the maximum of numeric values that are that are extracted from the aggregated documents.
Supported field types are `u64`, `f64`, `i64`, and `datetime`.

**Request**
```json skip
{
    "query": "*",
    "max_hits": 0,
    "aggs": {
        "max_price": {
            "max": { "field": "price" }
        }
    }
}
```

**Response**
```json
{
    "num_hits": 9582098,
    "hits": [],
    "elapsed_time_micros": 101543,
    "errors": [],
    "aggregations": {
        "max_price": {
            "value": 1353.23
        }
    }
}
```

### Min

A single-value metric aggregation that computes the minimum of numeric values that are that are extracted from the aggregated documents.
Supported field types are `u64`, `f64`, `i64`, and `datetime`.

**Request**
```json skip
{
    "query": "*",
    "max_hits": 0,
    "aggs": {
        "min_price": {
            "min": { "field": "price" }
        }
    }
}
```

**Response**
```json
{
    "num_hits": 9582098,
    "hits": [],
    "elapsed_time_micros": 102342,
    "errors": [],
    "aggregations": {
        "min_price": {
            "value": 0.01
        }
    }
}
```

### Stats

A multi-value metric aggregation that computes stats (average, count, min, max, standard deviation, and sum) of numeric values that are extracted from the aggregated documents.
Supported field types are `u64`, `f64`, `i64`, and `datetime`.

**Request**
```json skip
{
    "query": "*",
    "max_hits": 0,
    "aggs": {
        "timestamp_stats": {
            "stats": { "field": "timestamp" }
        }
    }
}
```



**Response**
```json
{
    "num_hits": 10000783,
    "hits": [],
    "elapsed_time_micros": 65297,
    "errors": [],
    "aggregations": {
        "timestamp_stats": {
            "avg": 1462320207.9803998,
            "count": 10000783,
            "max": 1475669670.0,
            "min": 1440670432.0,
            "standard_deviation": 11867304.28681695,
            "sum": 1.4624347076526848e16
        }
    }
}
```

### Sum

A single-value metric aggregation that that sums up numeric values that are that are extracted from the aggregated documents.
Supported field types are `u64`, `f64`, `i64`, and `datetime`.

**Request**
```json skip
{
    "query": "*",
    "max_hits": 0,
    "aggs": {
        "total_price": {
            "sum": { "field": "price" }
        }
    }
}
```

**Response**
```json
{
    "num_hits": 9582098,
    "hits": [],
    "elapsed_time_micros": 101142,
    "errors": [],
    "aggregations": {
        "total_price": {
            "value": 12966782476.54
        }
    }
}
```


### Percentiles
The percentiles aggregation is a useful tool for understanding the distribution of a data set.
It calculates the values below which a given percentage of the data falls.
For instance, the 95th percentile indicates the value below which 95% of the data points can be found.

This aggregation can be particularly interesting for analyzing website or service response times.
For example, if the 95th percentile website load time is significantly higher than the median, this indicates
that a small percentage of users are experiencing much slower load times than the majority.

To use the percentiles aggregation, you'll need to provide a field to aggregate on.
In the case of website load times, this would typically be a field containing the duration of time it takes for the site to load.

**Request**
```json skip
{
    "query": "*",
    "max_hits": 0,
    "aggs": {
        "loading_times": {
            "percentiles": {
                "field": "load_time"
                "percents": [90, 95, 99]
            }
        }
    }
}
```

**Response**
```JSON
{
    "num_hits": 9582098,
    "hits": [],
    "elapsed_time_micros": 101142,
    "errors": [],
    "aggregations": {
        "loading_times": {
            "values": {
                "90.0": 33.4,
                "95.0": 83.4,
                "99.0": 230.3
            }
        }
    }
}
```

`percents` may be omitted, it will default to `[1, 5, 25, 50 (median), 75, 95, and 99]`.

#### Estimating Percentiles

While percentiles provide valuable insights into the distribution of data, it's important to understand that they are often estimates.
This is because calculating exact percentiles for large data sets can be computationally expensive and time-consuming.




