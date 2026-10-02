# LiveKit deployment behind Docker

Accord connects clients to `LIVEKIT_EXTERNAL_URL` for signaling. Audio uses
separate WebRTC media addresses advertised by LiveKit. A working HTTPS proxy or
a speaking indicator does not establish that incoming audio can reach a client.

For a single LiveKit container behind a Docker bridge, advertise the host's
reachable IPv4 at the published media ports. Set `LIVEKIT_NODE_IP` in your
Compose `.env` file or Portainer stack environment variables. Use the public
IPv4 for internet clients, the host's LAN IPv4 for LAN-only clients, or
`127.0.0.1` only when all clients run on the Docker host.

The provided Compose files require this variable, pass it through `--node-ip`,
and load [livekit.yaml](../livekit.yaml), which sets `use_external_ip: false`.
`LIVEKIT_NODE_IP` is interpolated by Compose; it is not an Accord server
environment variable. Portainer stacks can use the equivalent inline
configuration below instead of a mounted config file.

```yaml
livekit:
  image: livekit/livekit-server:latest
  command: ["--keys", "${LIVEKIT_API_KEY}: ${LIVEKIT_API_SECRET}"]
  environment:
    LIVEKIT_CONFIG: |
      port: 7880
      rtc:
        node_ip: ${LIVEKIT_NODE_IP:?Set LIVEKIT_NODE_IP to the host IPv4}
        use_external_ip: false
        tcp_port: 7881
        udp_port: 7882
  ports:
    - "7881:7881/tcp"
    - "7882:7882/udp"
```

Retain the stack's existing network and reverse-proxy labels. Use matching
`LIVEKIT_API_KEY` and `LIVEKIT_API_SECRET` credentials in Accord and LiveKit.
An explicit `node_ip` takes effect with `use_external_ip: false`.
`udp_port` selects a single UDP media port, so this configuration does not need
the default UDP range `50000–60000`.

| Port | Purpose | Exposure |
| --- | --- | --- |
| 7880/TCP | HTTP/WebSocket signaling | Reverse proxy to this container port; publish it directly for local `ws://` examples |
| 7881/TCP | WebRTC TCP fallback | Publish and allow through the host firewall |
| 7882/UDP | WebRTC media | Publish and allow through the host firewall |
| 443/TCP | HTTPS/WebSocket TLS | Serve `wss://livekit.example.com` through the reverse proxy |

Forward the media ports to the Docker host when it is behind a router. DNS for
the media host must resolve to the reachable server; an HTTP-only proxy cannot
forward these WebRTC ports. Set Accord's `LIVEKIT_INTERNAL_URL` to
`http://livekit:7880` and `LIVEKIT_EXTERNAL_URL` to the client-facing signaling
URL. The latter does not override LiveKit's advertised media IP.

## Applying the change in Portainer

Open the stack's Editor, remove LiveKit's `--dev` argument, and add the
`LIVEKIT_CONFIG` block above to that service. Preserve its key arguments,
networks, ports, and proxy labels. If the service already uses `--config`,
configure `use_external_ip: false` in that file and add `--node-ip` with the
host address, or remove `--config` when switching to inline configuration.
Set `LIVEKIT_NODE_IP`, then update the stack.
The LiveKit restart interrupts current voice calls.

## Checking voice connectivity

Repeat joins and channel switches from a remote client. Check that both
publisher and subscriber ICE connections succeed and select the advertised
host address at port 7882/UDP or 7881/TCP. Confirm the microphone publishes and
that another participant's audio is audible.

Intermittent 20-second joins, successful joins only after retries, or silent
audio with speaking indicators can indicate incorrect media addresses. Inspect
the advertised ICE candidates: private Docker addresses such as `172.20.0.5`
are unreachable to internet clients. Development mode behind a bridge with
only fixed media ports published caused this failure in the Daccord deployment;
the explicit public address and ports resolved the reproduced connection failures.

Clients that require relay-only voice also need a reachable TURN service; this
direct-media configuration does not provide one.

References: [LiveKit deployment](https://docs.livekit.io/transport/self-hosting/deployment/),
[ports and firewall](https://docs.livekit.io/transport/self-hosting/ports-firewall/),
and [configuration fields](https://github.com/livekit/livekit/blob/master/config-sample.yaml).
