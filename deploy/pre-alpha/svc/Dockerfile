# syntax=docker/dockerfile:1.7
# The hosted pre-alpha's service (README.md). Build context: the ogun repository root.
#   docker build -f deploy/pre-alpha/svc/Dockerfile -t ogun-prealpha-svc:local .
FROM node:22-bookworm-slim AS build
WORKDIR /src
COPY apps/studio/web/package.json apps/studio/web/package-lock.json apps/studio/web/
RUN cd apps/studio/web && npm ci --no-audit --no-fund
COPY apps/studio/web apps/studio/web
COPY deploy/pre-alpha/svc deploy/pre-alpha/svc
RUN node deploy/pre-alpha/svc/build.mjs

FROM node:22-bookworm-slim
RUN install -d -o 10001 -g 10001 -m 0700 /data
COPY --from=build /src/deploy/pre-alpha/svc/dist/svc.mjs /app/svc.mjs
USER 10001:10001
ENV NODE_ENV=production SVC_DB=/data/svc.db SVC_PORT=8090
EXPOSE 8090
CMD ["node", "--disable-warning=ExperimentalWarning", "/app/svc.mjs"]
