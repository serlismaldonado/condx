# La app del teléfono (Android)

Condx en el teléfono: habla con los agentes, aprueba lo que te espera y recibe avisos desde
cualquier lugar. Es de código abierto:
[condx-mobile](https://github.com/serlismaldonado/condx-mobile).

## Qué necesita

- Un teléfono Android (la APK es para arm64, que cubre casi todos los teléfonos de los últimos años).
- **Condx Cloud** vinculado en la Mac. El teléfono y la Mac hablan a través de él, así que el
  teléfono funciona con datos móviles y con la Mac dormida. Se configura en unos minutos: ver
  [Condx Cloud y el sitio en la nube](16-nube.md).

## Instalar

1. En el teléfono, abre el
   [último release](https://github.com/serlismaldonado/condx-mobile/releases/latest) y baja
   `condx-mobile-<versión>-arm64.apk`.
2. Abre el archivo. Android pide una vez permitir instalaciones desde el navegador (**Instalar apps
   desconocidas**): permítelo y toca **Instalar**.
3. Opcional: cada release trae un archivo `.sha256` junto a la APK para verificar la descarga.

Las actualizaciones llegan solas: cuando sale una versión nueva, la app muestra un aviso con
**Instalar** (y una notificación). Van firmadas con la misma llave, así que se instalan encima de la
anterior y conservan el emparejamiento.

## Vincularlo con la Mac

1. En la Mac: **Condx → Configuración → Condx Cloud → Vincular un teléfono**. Aparecen un código QR y un
   código. El código sirve una sola vez y caduca en 5 minutos.
2. En el teléfono: abre Condx y escanea el QR (o pega el enlace).
3. De vuelta en la Mac, comprueba que la huella coincide con la que muestra el teléfono y toca
   **Confirmar**.

El teléfono crea su propia llave y la guarda en el Keystore de Android. Nunca sale del teléfono:
cada mensaje y cada aprobación que mandas va firmado con ella, y la Mac y Condx Cloud revisan la
firma.

## Qué puedes hacer

| | |
| --- | --- |
| **Hilos** | Conversa con los agentes. Responde a un mensaje y la respuesta le llega a quien lo escribió; escribe `@` para sumar a alguien más. |
| **Aprobaciones** | Despliegues, releases y merges que te esperan, con un deslizador. Producción además pide Touch ID en la Mac. |
| **Ver cambios** | El diff de lo que cambió un agente, antes de aprobar. |
| **Notas de voz** | Habla en vez de escribir. Se transcriben en tu Mac con Whisper: el audio no sale de casa. |
| **Fotos y archivos** | Una captura del error, un log, un PDF. Los agentes que ven imágenes los leen. |
| **Notificaciones** | Respuestas, incidentes y aprobaciones, como push y en la campana de la app. Toca una para abrir su hilo. |

## Con la Mac apagada

El teléfono conserva el último estado. Los mensajes que escribas esperan en Condx Cloud y se
entregan cuando vuelve la Mac. Si configuraste el sitio en la nube (ver [16-nube.md](16-nube.md)),
los agentes contestan en el momento. Las notas de voz esperan a la Mac: van selladas para ella.

## Desvincular o perder el teléfono

- **En el teléfono:** **Desvincular este teléfono**.
- **En la Mac:** Condx → Configuración → Condx Cloud, **Quitar** junto al teléfono. Hazlo si pierdes el teléfono:
  desde ese momento no se acepta nada firmado con su llave.

## Compilarla tú

El [README del repo](https://github.com/serlismaldonado/condx-mobile#readme) tiene los pasos
(`nsh`, el SDK y el NDK de Android).
