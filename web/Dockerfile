FROM node:20-alpine AS build
WORKDIR /app
RUN apk add --no-cache python3 make g++ libc6-compat
COPY package.json ./
RUN npm install --production

FROM node:20-alpine
RUN apk add --no-cache libc6-compat
WORKDIR /app
COPY --from=build /app/node_modules ./node_modules
COPY server.js ./
COPY public ./public
EXPOSE 3000
CMD ["node", "server.js"]
