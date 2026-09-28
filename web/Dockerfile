# The management interface as a container image. Build from this directory:
#   docker build -t rustgit-web .
# It listens on port 3000 and needs RUSTGIT_URL, RUSTGIT_PUBLIC_URL, and
# SESSION_SECRET at run time (README.md).

FROM node:24-alpine AS deps
WORKDIR /app
COPY package.json package-lock.json ./
RUN npm ci --no-audit --no-fund

FROM node:24-alpine AS build
WORKDIR /app
ENV NEXT_TELEMETRY_DISABLED=1
COPY --from=deps /app/node_modules ./node_modules
COPY . .
RUN npm run build

FROM node:24-alpine
WORKDIR /app
ENV NODE_ENV=production NEXT_TELEMETRY_DISABLED=1 PORT=3000 HOSTNAME=0.0.0.0
COPY --from=build --chown=10002:10002 /app/.next/standalone ./
COPY --from=build --chown=10002:10002 /app/.next/static ./.next/static
USER 10002:10002
EXPOSE 3000
CMD ["node", "server.js"]
