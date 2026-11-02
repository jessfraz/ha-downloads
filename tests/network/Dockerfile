# Build with the already pinned production image. Only disposable test trust is added.
ARG APP_IMAGE=ha-downloads:smoke
FROM ${APP_IMAGE}
RUN apk add --no-cache openssl openvpn iptables
COPY fixture.py probe.py /fixture/
COPY cert.pem key.pem /fixture/
RUN cat /fixture/cert.pem >> /etc/ssl/certs/ca-certificates.crt
