FROM localhost:5001/ax-task-runner:latest
COPY node /opt/node
COPY cli /opt/cli
COPY codex-home /opt/codex-home
COPY run-agent.sh /usr/local/bin/run-agent.sh
RUN ln -sf /opt/node/bin/node /usr/local/bin/node \
 && ln -sf /opt/cli/node_modules/.bin/codex /usr/local/bin/codex \
 && chmod +x /usr/local/bin/run-agent.sh
ENV CODEX_HOME=/opt/codex-home
