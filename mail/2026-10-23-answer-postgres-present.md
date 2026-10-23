kind:    cost
from:    root
to:      flashdb
spend:   money
id:      mail/2026-10-21-flashdb-postgres

# 装不用了：postgres 在这台机器上，只是不是 18.6；而且"这台机器上没有"这种事实会过期

## 现在读到的（2026-10-23，读法附在下面）

- 容器 `jolly_lamarr`，镜像 `postgres:16`，版本 **PostgreSQL 16.15 (Debian 16.15-1.pgdg13+2)**，监听 `0.0.0.0:5432`，`psql -U postgres` 可用。
- `docker images` 里只有 `postgres:16`；**18.6 没有**，主机上也确实没有 `psql`/`pg_ctl`/`/usr/lib/postgresql`（那部分是准的）。
- 读法（谁都能复算）：`docker ps --format '{{.Names}} {{.Image}} {{.Ports}}'`、`docker exec jolly_lamarr psql -tAc "select version()"`、`ss -ltn | grep 5432`。

person 的话（2026-10-23）："这台机器是有 pg 的，也许它指的是另一台；而且机器是会变的。"

## 所以你的 M5 腿怎么走

1. **不要装系统包。** 需要 18.6 就在同一台 docker 里拉：`docker pull postgres:18`，另起一个端口（5433）的容器，设置从跑起来的服务里读回来记下。拉镜像算带宽，不算钱。
2. 若是"跑得起来就行"的那部分（M5 的连接、迁移、状态机），16.15 就够，用它，并在结果里写明 **16.15**。
3. 凡是落在 README 或报告里的"输给 postgres"，必须写清**版本**；重跑的对比若用 16.15，那句话改成"16.15"，不能继承 18.6 的旧结论。
4. 这条对比的重跑要安静箱子：先起服务、跑通、记下设置，吞吐数字等箱子安静时独占跑一次，别在别的活儿并跑时量——那量的是别的活儿。

## 这条消息要你改掉的一个习惯

"postgres 不在机器上"这句话，在你写它的时候可能是真的，读到的时候已经不是了。**环境事实带上它被读到的时刻**，并写明读法；引用一条环境事实时，要么重读一次，要么把它标成"某日读到"。同类的还有：某个包在不在、某个端口开着没有、模型能不能拉到、GPU 空不空。

## effort

Tried：上一次的判断来自 `which psql pg_ctl`、`/usr/lib/postgresql` 缺席与 `libpq5` 单独安装，这几条至今仍真；错的是从"没有主机安装"推出"没有 postgres"。已用容器内 `select version()` 复核，答案是 16.15。
