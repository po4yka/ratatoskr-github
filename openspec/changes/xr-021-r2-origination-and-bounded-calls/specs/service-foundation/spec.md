## ADDED Requirements

### Requirement: Publisher health reaches readiness

The service SHALL keep one `bus_publish` readiness check fed by the outbox relay. The check SHALL be unhealthy when the last pass attempted a publish, at least one publish failed, and the oldest unpublished publishable row has three or more attempts or is older than 300 seconds, and healthy again once the oldest unpublished publishable row is no longer stuck or none waits (a pass that only publishes later rows while a stuck row backs off keeps the check unhealthy). A row whose envelope exceeds the connected server's `max_payload` minus 1 KiB SHALL be marked undeliverable at once, SHALL never count towards readiness, and SHALL NOT stop the relay.

#### Scenario: A refused subject flips readiness and recovers

- **WHEN** the broker's ACL refuses one relayed subject and its row fails three attempts
- **THEN** `/ready` answers 503 with a `bus_publish` check, other rows still publish, and the next clean pass makes `/ready` answer 200

#### Scenario: An oversize row never flips readiness

- **WHEN** a row is larger than the broker's payload limit
- **THEN** the row is marked undeliverable and readiness stays 200
