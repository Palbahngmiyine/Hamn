#ifndef HAMN_AGENT_ROUTER_H
#define HAMN_AGENT_ROUTER_H

struct conn;
struct http_req;

void router_dispatch(struct conn *c, struct http_req *r);

#endif
