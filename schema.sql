create table todos (
  id bigserial,
  title text not null,
  "order" bigint not null default 0,
  completed boolean not null default false
);
