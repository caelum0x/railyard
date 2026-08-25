# Aeonrift — Localisation des messages d'erreur / d'état (fr)
#
# Chaînes d'erreur et d'état destinées à l'utilisateur pour le client et les services.
# Gardez des messages exploitables ; ne jamais divulguer de détails internes.

## Connectivité

error-network-lost = Connexion perdue. Tentative de reconnexion…
error-network-timeout = Le serveur n'a pas répondu. Veuillez vérifier votre connexion.
error-server-unavailable = Les serveurs sont indisponibles pour le moment. Veuillez réessayer plus tard.
error-reconnecting = Reconnexion… ({ $attempt } sur { $max })
error-reconnect-failed = Impossible de se reconnecter à la partie.

## Authentification

error-auth-required = Vous devez être connecté pour continuer.
error-auth-invalid = Nom d'utilisateur ou mot de passe incorrect.
error-auth-expired = Votre session a expiré. Veuillez vous reconnecter.
error-auth-banned = Ce compte est suspendu.

## Recherche de partie

error-match-declined = Partie annulée : un joueur a refusé.
error-match-timeout = Partie annulée : un joueur n'a pas répondu.
error-match-abandoned = Vous avez quitté une partie et pourriez recevoir une pénalité.
error-queue-dodge = Un joueur s'est esquivé. Retour au salon.

## Sélection de champion / jeu

error-champion-locked = Ce champion n'est pas disponible.
error-champion-banned = Ce champion a été banni cette partie.
error-champion-already-picked = Ce champion est déjà pris.
error-invalid-target = Cible invalide.
error-out-of-range = La cible est hors de portée.
error-not-enough-mana = Mana insuffisant.
error-not-enough-energy = Énergie insuffisante.
error-ability-on-cooldown = Compétence en recharge ({ $seconds }s).

## Boutique / économie

error-not-enough-gold = Or insuffisant.
error-item-not-available = Cet objet n'est pas disponible.
error-inventory-full = Votre inventaire est plein.

## Contenu / chargement

error-content-load-failed = Échec du chargement du contenu du jeu. Veuillez redémarrer le client.
error-content-corrupt = Les fichiers du jeu semblent corrompus. Une réparation est recommandée.
error-version-mismatch = Votre client est obsolète. Veuillez le mettre à jour pour continuer.

## Générique

error-unknown = Une erreur est survenue. Veuillez réessayer.
error-retry = Réessayer
error-report = Signaler un problème
error-details = Code d'erreur : { $code }
