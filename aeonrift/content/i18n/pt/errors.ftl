# Aeonrift — Localização de mensagens de erro / status (pt)
#
# Strings de erro e status voltadas ao usuário para o cliente e os serviços.
# Mantenha as mensagens acionáveis; nunca exponha detalhes internos.

## Conectividade

error-network-lost = Conexão perdida. Tentando reconectar…
error-network-timeout = O servidor não respondeu. Verifique sua conexão.
error-server-unavailable = Os servidores estão indisponíveis no momento. Tente novamente mais tarde.
error-reconnecting = Reconectando… ({ $attempt } de { $max })
error-reconnect-failed = Não foi possível reconectar à partida.

## Autenticação

error-auth-required = Você precisa estar conectado para continuar.
error-auth-invalid = Nome de usuário ou senha incorretos.
error-auth-expired = Sua sessão expirou. Entre novamente.
error-auth-banned = Esta conta está suspensa.

## Matchmaking

error-match-declined = Partida cancelada: um jogador recusou.
error-match-timeout = Partida cancelada: um jogador não respondeu.
error-match-abandoned = Você abandonou uma partida e pode receber uma penalidade.
error-queue-dodge = Um jogador desistiu. Voltando ao lobby.

## Seleção de campeão / jogabilidade

error-champion-locked = Esse campeão não está disponível.
error-champion-banned = Esse campeão foi banido nesta partida.
error-champion-already-picked = Esse campeão já foi escolhido.
error-invalid-target = Alvo inválido.
error-out-of-range = O alvo está fora de alcance.
error-not-enough-mana = Mana insuficiente.
error-not-enough-energy = Energia insuficiente.
error-ability-on-cooldown = Habilidade em recarga ({ $seconds }s).

## Loja / economia

error-not-enough-gold = Ouro insuficiente.
error-item-not-available = Esse item não está disponível.
error-inventory-full = Seu inventário está cheio.

## Conteúdo / carregamento

error-content-load-failed = Falha ao carregar o conteúdo do jogo. Reinicie o cliente.
error-content-corrupt = Os arquivos do jogo parecem estar corrompidos. Recomenda-se um reparo.
error-version-mismatch = Seu cliente está desatualizado. Atualize para continuar.

## Genérico

error-unknown = Algo deu errado. Tente novamente.
error-retry = Tentar Novamente
error-report = Relatar um Problema
error-details = Código de erro: { $code }
