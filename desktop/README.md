# yggproxy для Linux

Один бинарник: служба (сервер выхода и VPN-клиент), консольные команды и окно (egui).
`tcp_node` и `ygghelper` встроены в него и при запуске службы распаковываются в
`/var/lib/yggproxy/bin`. Порт прокси в Yggdrasil у всех серверов один — **27080**, поэтому
клиенту нужен только адрес сервера; телефоны с YggProxy и компьютеры совместимы друг с другом.

## Установка

```sh
sudo ./yggproxy install     # /usr/local/bin/yggproxy, служба systemd, группа yggproxy
```

Пользователь, запустивший `sudo`, попадает в группу `yggproxy` и после повторного входа
управляет службой без `sudo`. Удаление: `sudo yggproxy uninstall` (настройки и ключ узла
остаются в `/etc/yggproxy` и `/var/lib/yggproxy`).

## Сервер (консоль)

```sh
yggproxy server on                # раздавать интернет
yggproxy qr                       # адрес и QR-код для клиента (телефон сканирует его)
yggproxy limit client 20 5        # каждому клиенту ↓20 / ↑5 Мбит/с (0 — без ограничения)
yggproxy limit total 100 50       # весь сервер
yggproxy clients                  # клиенты и их трафик
yggproxy status · peers · logs -f
```

Ограничения применяются сразу, без обрыва соединений. Клиента сервер узнаёт по его адресу
Yggdrasil (tcp_node передаёт его прокси строкой PROXY v1, адрес проверен по ключу).

## Клиент

```sh
yggproxy connect 200:…            # или ссылка yggproxy://c?a=…
yggproxy speedtest                # 10 с приём, 10 с отдача до сервера
yggproxy disconnect
```

или окно: `yggproxy gui` (пункт «YggProxy» в меню приложений).

Как устроен VPN: TUN `yggproxy0` → `ygghelper tun` (tun2proxy) → `127.0.0.1:10808` →
`tcp_node --connect` → Yggdrasil → сервер. Маршрутизация через правила:

| приоритет | правило |
|---|---|
| 8999 | `lookup main suppress_prefixlength 0` — локальные сети и линки как обычно |
| 9000 | `fwmark 0x2a7 lookup main` — соединения узла с пирами идут мимо туннеля |
| 9001 | `lookup 2701` — всё остальное: `default dev yggproxy0` |

DNS — через systemd-resolved (`resolvectl dns yggproxy0 10.111.222.53`, домен `~.`), его
отвечает виртуальный DNS tun2proxy, имена разрешает сервер. Без IPv6 в настройках IPv6
блокируется (а не идёт мимо туннеля). При остановке службы правила и маршруты убираются.

Клиент сам ищет пиров с наименьшим пингом до сервера: `yggproxy set rtt_limit 100`,
`swap_interval 30`, `swap_settle 60` (то же — вкладка «Сеть»).

## Сборка

```sh
# tcp_node (из yggdrasil-lite) и ygghelper для хоста
(cd ~/yggdrasil-lite && cargo build --release --bin tcp_node --features upstream-tls,html)
(cd ../ygghelper && cargo build --release)
cargo build --release                                            # с окном
cargo build --release --no-default-features --target-dir target/server   # для серверов, без GUI
```

Пути к встраиваемым бинарникам можно задать: `TCP_NODE_BIN=… YGGHELPER_BIN=…`.
Для проверки окна без экрана: `YGGPROXY_SHOT=папка yggproxy gui` снимает все вкладки в PPM и
закрывается. Для запуска нескольких экземпляров (тесты в netns): `YGGPROXY_CONFIG`,
`YGGPROXY_STATE`, `YGGPROXY_SOCKET`.
