# Condx Cloud y el sitio en la nube (opcional)

Condx funciona solo con la Mac. Dos piezas opcionales lo llevan más lejos:

| Nivel | Qué ganas | Cuánto cuesta |
| --- | --- | --- |
| **Solo la Mac** | Todo lo de la app de escritorio | Nada que configurar |
| **+ Condx Cloud** | La [app del teléfono](15-telefono.md), Claude desde cualquier lugar (MCP remoto) y el estado visible con la Mac dormida | Un proyecto de Convex tuyo; el plan gratis alcanza para una persona |
| **+ Sitio en la nube en boat** | Los agentes siguen contestando, arreglando y desplegando a cloud dev con la Mac apagada | Un sandbox de [boat](https://boat.dev): el pequeño entra en el mínimo de 20 USD al mes |

**Producción nunca sale de la Mac.** Promover y revertir solo pasan ahí, con Touch ID.

## Condx Cloud

Condx Cloud es una app chica de Convex (`cloud/` en este repo) que corre **en tu propia cuenta de
Convex**. La Mac, el teléfono y el sitio en la nube se sincronizan a través de ella. Nadie más
guarda tus datos.

### Configurarlo

1. En el [dashboard de Convex](https://dashboard.convex.dev), crea un proyecto (por ejemplo
   `condx-cloud`).
2. En ese proyecto: **Settings → Deploy keys → Generate production deploy key**.
3. Desde un clon de este repo (necesita `bun` y Node.js), corre:

   ```bash
   scripts/cloud-deploy.sh
   ```

   Pide la key sin mostrarla, despliega `cloud/` y vincula el daemon de esta Mac. La key se usa una
   vez y no se guarda.
4. Empareja tu teléfono: [15-telefono.md](15-telefono.md).

Compruébalo con `~/.condx/daemon/bin/condxd cloud status`.

### Tu propio dominio (opcional)

El QR y la página de emparejamiento usan `<deployment>.convex.site` por defecto. Para mostrar tu
propio dominio (por ejemplo `condx.example.com`), pon delante un proyecto chico de Vercel:

```bash
CONDX_SITE_DOMAIN=condx.example.com scripts/cloud-site-proxy.sh https://<deployment>.convex.site
~/.condx/daemon/bin/condxd cloud site https://condx.example.com
```

Después agrega en tu proveedor de DNS el CNAME que muestra Vercel.

### Actualizarlo

Cuando una versión nueva de Condx cambia `cloud/`, vuelve a desplegarlo sin re-emparejar nada.
Guarda la deploy key una vez:

```bash
~/.condx/daemon/bin/condxd secret set condx-cloud-key
```

Desde entonces lo despliega la receta **deploy-condx-cloud**, que espera tu aprobación en la app o
en el teléfono. A mano: `CONDX_DEPLOY_ONLY=1 scripts/cloud-deploy.sh`.

## El sitio en la nube en boat

El sitio en la nube es el mismo daemon (`condxd`) compilado para Linux, corriendo en un sandbox de
[boat](https://boat.dev). Cuando la Mac no está (3 minutos sin latido, o se apagó), contesta los
mensajes del teléfono, corre los agentes y despliega a **cloud dev** y a **previews de Vercel**. No
tiene entornos de producción.

Cuando vuelve la Mac, sus hilos regresan a ella junto con lo que hizo el sitio mientras tanto.

### Configurarlo

1. En boat, crea un sandbox. `small` (2 vCPU, 4 GB) alcanza.
2. Dentro (por SSH o `boat exec`), construye y arranca el sitio:

   ```bash
   git clone https://github.com/serlismaldonado/condx.git && cd condx
   docker build -f deploy/site/Dockerfile -t condx-site .
   docker run -d --name condx-site --restart unless-stopped --network host -v condx-site:/data condx-site
   ```

3. Emparéjalo como un teléfono. En la Mac, abre **Configuración → Condx Cloud → Vincular un teléfono**,
   copia el enlace del QR y, dentro del sandbox, corre:

   ```bash
   docker exec condx-site condxd site pair "<el enlace del QR>"
   ```

   Confírmalo en la Mac: aparece en **Sitio en la nube**, aparte de los teléfonos. La Mac le manda
   entonces tus ajustes de agentes y las claves de los proveedores, selladas para que solo el sitio
   las abra.

4. Déjalo clonar tus repos. Usa un token fine-grained de GitHub del dueño del repo, con contents y
   pull requests en escritura. Sube ramas `condx/*` y abre PRs; nunca toca `main`.

   ```bash
   docker exec -i condx-site condxd secret set github-token:<dueño>
   ```

   Para agentes con **Claude Code**: corre `claude setup-token` en tu Mac y pégalo en los ajustes
   de agentes: edita el proveedor **Claude Code**, campo **Token (opcional)**. Le llega al sitio cifrado. El sitio
   no tiene sesión propia de Claude Code.

### Qué recibe (lo mínimo)

| Recibe | Nunca recibe |
| --- | --- |
| Deploy keys de cloud dev, claves de proveedores, el token de GitHub que le des | Deploy keys de producción |
| Tus ajustes de agentes (modelo por defecto, presupuesto diario, agentes a la vez) | El Llavero de tu Mac |
| Los teléfonos en los que confía la Mac, para revisar sus firmas | Poder aprobar: las aprobaciones siguen en tu teléfono o en la Mac |

Para quitarlo: en la Mac, **Quitar** junto a él en **Sitio en la nube**. Dentro del sandbox,
`docker exec condx-site condxd site unpair`.
