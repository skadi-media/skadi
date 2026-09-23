Feature: HTTP egress client — proxy selection and a dead proxy (C05)
  In the deploy stack indexer searches must egress through gluetun's HTTP proxy
  (the VPN). The shared client has no explicit proxy setting; the only way it
  would use one is reqwest's ambient HTTP_PROXY environment, which the daemon
  container does not set — so today only the native cardigann fetcher is
  proxied (it builds its own reqwest client from `cardigann_proxy_url`).

  @C05 @passing @serial
  Scenario: when the ambient proxy is down every request through it fails fast
    Given the ambient HTTP proxy is "http://127.0.0.1:9"
    And an egress client built under that proxy with 2 retries and a 1 ms base backoff
    And the upstream answers "/via-proxy" with 200 and body "unreachable"
    When the client GETs "/via-proxy"
    Then the request fails with a network error
    And the request took less than 5000 ms

  @C05 @passing @SKADI-T-0522
  Scenario: the proxy is an explicit client setting rather than ambient process environment
    Given an egress client with the default retry policy
    Then the client can be configured to egress via the proxy "http://gluetun:8888"
