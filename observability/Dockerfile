FROM grafana/otel-lgtm:0.5.0

USER root

COPY otelcol-config.yaml /etc/otelcol/config.yaml

ENV OTELCOL_CONFIG=/etc/otelcol/config.yaml
ENV GF_SERVER_HTTP_PORT=3300

EXPOSE 4317 4318 3100 3300
