# yggproxy для Linux и Windows

[← Ratatoskr](../README.md)

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
yggproxy servers                  # серверы: сохранённые и от трекеров, с пингом
yggproxy connect                  # к серверу с наименьшим пингом
yggproxy connect 200:…            # или к конкретному (ссылка yggproxy://c?a=… тоже подходит)
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

Служба всё время держит узел Yggdrasil (`yggproxy set keep_node off` — только при включённом
клиенте или сервере): раз в 10 с пингует серверы из списка и корень сети, раз в 2 мин спрашивает
трекеры (`yggproxy set trackers АДРЕС,АДРЕС`; новые трекеры добавляются сами), раз в час берёт у
них проверенные публичные пиры. В окне — то же на вкладках «Клиент» и «Сеть».

Клиент сам ищет пиров с наименьшим пингом до сервера: `yggproxy set rtt_limit 100`,
`swap_interval 30`, `swap_settle 60` (то же — вкладка «Сеть»).

## Трекер

Трекер ищет серверы выхода Ratatoskr по всей сети Yggdrasil и раздаёт их список клиентам. Ему
нужен обычный yggdrasil-go на той же машине (TUN и admin-сокет).

```sh
sudo yggproxy install-tracker                        # служба yggproxy-tracker
sudo ufw allow in on tun0 to any port 27081 proto tcp   # чтобы трекер был виден из сети
journalctl -u yggproxy-tracker -f
yggproxy exits [ТРЕКЕР]                              # список серверов от трекера
yggproxy trackers [ТРЕКЕР]                           # известные трекеры
```

Как работает: через admin API (`debug_remotegetpeers`) обходит сеть от своих пиров, у каждого
узла проверяет порт 27080 (SOCKS5 и CONNECT на порт теста скорости) и порт трекера 27081.
API на `[адрес Yggdrasil]:27081`: `GET /exits`, `GET /trackers`, `GET /info`. Списки других
трекеров забираются и перепроверяются. Настройки и расписание — `/etc/yggproxy/tracker.json`:

| Ключ | По умолчанию | Что |
|---|---|---|
| `crawl_interval_min` | 30 | полный обход сети и проверка портов |
| `recheck_interval_min` | 5 | перепроверка найденных серверов и трекеров |
| `sync_interval_min` | 5 | обмен списками с другими трекерами |
| `hide_after_fails` | 2 | скрыть сервер или трекер после стольких неудачных проверок подряд (0 — только по `expire_hours`) |
| `expire_hours` | 24 | сколько держать сервер, который не отвечает |
| `crawl_workers` / `probe_workers` | 24 / 64 | параллельность обхода и проверки |
| `trackers` | сервер проекта | с кого начинать синхронизацию |

Если обход находит всего пару десятков узлов, у yggdrasil-go плохие пиры: удалённые запросы через
них не проходят. Поменяйте `Peers` в `/etc/yggdrasil/yggdrasil.conf` на быстрые узлы из
[публичного списка](https://publicpeers.neilalexander.dev/).

## Windows

То же, что на Linux: служба, консольные команды, окно, трекер.

| | Linux | Windows |
|---|---|---|
| установка | `sudo ./yggproxy install` | запустить `yggproxy-windows-x86_64.exe` → «Установить и запустить службу» (или `yggproxy install` — спросит права администратора) |
| служба | systemd `yggproxy` | служба Windows **Ratatoskr** (LocalSystem, автозапуск, перезапуск при сбое) |
| программа | `/usr/local/bin/yggproxy` | `C:\Program Files\Ratatoskr\yggproxy.exe` (папка добавляется в PATH), ярлык в «Пуске» |
| настройки и ключ | `/etc/yggproxy`, `/var/lib/yggproxy` | `C:\ProgramData\Ratatoskr` |
| управление службой | unix-сокет, группа `yggproxy` | `127.0.0.1:27079` |
| трекер | `sudo yggproxy install-tracker` | `yggproxy install-tracker` → служба **RatatoskrTracker**, порт 27081 открывается в брандмауэре; журнал `C:\ProgramData\Ratatoskr\state\tracker.log` |
| удаление | `sudo yggproxy uninstall` | `yggproxy uninstall` |

VPN на Windows: `ygghelper tun` создаёт адаптер Wintun `yggproxy0` (`wintun.dll` встроен в
программу и кладётся рядом с `ygghelper.exe`). Служба даёт адаптеру маршруты `0.0.0.0/1`,
`128.0.0.0/1`, `::/1`, `8000::/1` и DNS `10.0.0.1`; DNS-запросы уходят по TCP через туннель на
`8.8.8.8` (так узел получает настоящие адреса пиров). Соединения узла с пирами привязаны к
интерфейсу основного маршрута (`IP_UNICAST_IF`) и идут мимо туннеля. Маршруты и DNS живут, пока
жив адаптер: при выключении VPN или остановке службы они исчезают сами.

Трекеру на Windows нужен yggdrasil-go: он обращается к его admin API по
`tcp://localhost:9001` (`admin_socket` в `tracker.json`).

Ограничения Windows-версии: программа не подписана (SmartScreen предупредит при первом
запуске); DNS-запросы к серверу в локальной сети могут идти мимо туннеля; сервер выхода и VPN на
одном компьютере одновременно не разделены (трафик сервера пойдёт в туннель).

## Сборка

```sh
# tcp_node (из yggdrasil-lite) и ygghelper для хоста
(cd ~/yggdrasil-lite && cargo build --release --bin tcp_node --features upstream-tls,html)
(cd ../ygghelper && cargo build --release)
cargo build --release                                            # с окном
cargo build --release --no-default-features --target-dir target/server   # для серверов, без GUI
```

Пути к встраиваемым бинарникам можно задать: `TCP_NODE_BIN=… YGGHELPER_BIN=…` (для Windows ещё
`WINTUN_DLL=…`). Сборка для Windows — `cargo build --release --target x86_64-pc-windows-gnu`
(нужны `mingw-w64` и Windows-сборки `tcp_node.exe` и `ygghelper.exe`).
Для проверки окна без экрана: `YGGPROXY_SHOT=папка yggproxy gui` снимает все вкладки в PPM и
закрывается. Для запуска нескольких экземпляров (тесты в netns): `YGGPROXY_CONFIG`,
`YGGPROXY_STATE`, `YGGPROXY_SOCKET`.
