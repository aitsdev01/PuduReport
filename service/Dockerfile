# syntax=docker/dockerfile:1
# SPDX-License-Identifier: GPL-3.0-only
# Copyright (c) 2026 bc0d3
#
# Imagen de pudureport-service. Se construye desde la raiz del repositorio:
#
#   docker build -f service/Dockerfile -t pudureport-service .
#
# Imagenes base fijadas por digest; Typst descargado con version fija y
# verificacion SHA-256. La imagen final corre sin root, no necesita escribir
# fuera de /tmp (montar un volumen temporal con el sistema de archivos de solo
# lectura) y no hace llamadas de red.

ARG RUST_IMAGE=rust:1.93.0-slim-bookworm@sha256:776861219cd851131c1cec3bbd7cbeb16b99a794048097eb69ad9682a8ed0d57
ARG RUNTIME_IMAGE=debian:bookworm-slim@sha256:7ff6ba31527b9e336162fa061746c366c4b2032f4d2cff2078bea43f78efe3f8

# --- Compilacion -------------------------------------------------------------
FROM ${RUST_IMAGE} AS build
WORKDIR /src
# El workspace completo (los manifiestos de todos los miembros); solo se
# compila el servicio.
COPY Cargo.toml Cargo.lock ./
COPY core core
COPY templates templates
COPY service service
COPY mcp mcp
COPY src-tauri src-tauri
RUN cargo build --locked --profile release-service -p pudureport-service \
    && install -m 0755 target/release-service/pudureport-service /usr/local/bin/pudureport-service

# --- Typst (version fija, hash verificado) -----------------------------------
FROM ${RUNTIME_IMAGE} AS typst
ARG TARGETARCH
ARG TYPST_VERSION=v0.13.1
ARG TYPST_SHA256_AMD64=7d214bfeffc2e585dc422d1a09d2b144969421281e8c7f5d784b65fc69b5673f
ARG TYPST_SHA256_ARM64=4f5b7ee6e57fb639019ee0f6bffcf940edad228ede6ff5269a9f05a1544ceed4
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl xz-utils \
    && rm -rf /var/lib/apt/lists/*
RUN set -eu; \
    case "${TARGETARCH:-amd64}" in \
      amd64) arch=x86_64; sha="${TYPST_SHA256_AMD64}" ;; \
      arm64) arch=aarch64; sha="${TYPST_SHA256_ARM64}" ;; \
      *) echo "arquitectura no soportada: ${TARGETARCH}"; exit 1 ;; \
    esac; \
    name="typst-${arch}-unknown-linux-musl"; \
    curl --proto '=https' --tlsv1.2 -fsSL -o /tmp/typst.tar.xz \
      "https://github.com/typst/typst/releases/download/${TYPST_VERSION}/${name}.tar.xz"; \
    echo "${sha}  /tmp/typst.tar.xz" | sha256sum -c -; \
    tar -xJf /tmp/typst.tar.xz -C /tmp; \
    install -m 0755 "/tmp/${name}/typst" /usr/local/bin/typst; \
    /usr/local/bin/typst --version

# --- Imagen final ------------------------------------------------------------
FROM ${RUNTIME_IMAGE}
# Fuentes que nombran las plantillas (theme.typ): Liberation Sans (respaldo de
# Helvetica/Arial), JetBrains Mono y DejaVu Sans Mono. Licencias OFL/Bitstream.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
      fonts-liberation2 fonts-dejavu-core fonts-jetbrains-mono \
    && rm -rf /var/lib/apt/lists/*
COPY --from=typst /usr/local/bin/typst /usr/local/bin/typst
COPY --from=build /usr/local/bin/pudureport-service /usr/local/bin/pudureport-service
COPY LICENSE THIRD-PARTY-NOTICES.md /usr/share/doc/pudureport-service/

ENV PUDU_TYPST_BIN=/usr/local/bin/typst \
    PUDUREPORT_SERVICE_ADDR=0.0.0.0:8080 \
    PUDUREPORT_SERVICE_TMPDIR=/tmp \
    TMPDIR=/tmp

# Usuario sin privilegios (numerico, sin entrada en /etc/passwd).
USER 10001:10001
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/pudureport-service"]
