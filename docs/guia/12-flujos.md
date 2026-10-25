# Flujos de despliegue

Un **flujo** es la receta de cómo un proyecto llega de un commit a un entorno: qué pasos corren,
en qué orden, cuánto se espera y dónde hace falta tu aprobación. Se arman en la página **Flujos**
(barra lateral, grupo Proyecto), como en n8n: nodos unidos por flechas.

heskala-erp empieza con su promoción de siempre ya armada como flujo («Promoción a producción»:
respaldo → ensayo → cloud dev → esperar 30 min → promover). **Flujos → Nuevo → Crear sugeridos**
agrega, apagados, los sugeridos que le faltan al proyecto: desplegar a local, desplegar a cloud dev
(CI en verde, cloud dev responde, tu aprobación, despliegue, aviso), ensayo con datos de cloud dev,
revertir producción y respaldos de cloud.

## Los nodos

| Nodo | Qué hace | Qué se configura |
|---|---|---|
| **Receta** | Pide un trabajo de cualquier receta (respaldo, ensayo, despliegue, correr una función…) y espera a que termine. Si la receta pide aprobación, el nodo la espera. | Receta, entorno y un campo por argumento |
| **Espera** | Deja pasar unos minutos (p. ej. ver si aparecen errores antes de seguir). | Minutos |
| **Aprobación** | Se detiene hasta que lo apruebes en la app. *Reforzada*: hay que escribir el nombre del entorno; en producción, con Touch ID. | Entorno, nivel y mensaje |
| **Verificación** | Comprueba algo y falla si no se cumple: que el entorno responda, que haya un respaldo reciente o que el CI esté en verde. | Qué verificar, entorno, horas |
| **Aviso** | Deja un mensaje en la corrida. | Mensaje |

Cada nodo tiene además:

- **Interruptor de encendido:** apagado, se salta sin borrarlo.
- **Si falla:** *parar el flujo* (por defecto) o *seguir con el siguiente*.

En un nodo de receta, el **entorno** solo lista los entornos donde corre esa receta (se elige solo
si hay uno) y cada argumento tiene su campo. El bloque **Variables** explica qué se puede escribir
en ellos, y un clic copia una:

- `{run.commit}`: el commit o la rama que eliges al empezar la corrida;
- `{node.<id>.<salida>}`: lo que dejó un nodo anterior conectado, por ejemplo
  `{node.backup.backup_id}` (el número del respaldo que hizo el nodo `backup`).

## Armar un flujo

- **Nuevo:** desde una plantilla, en blanco, importando un JSON o con **Crear sugeridos**.
- **Agregar nodos:** con la paleta de arriba (Receta, Espera, Aprobación, Verificación, Aviso).
- **Mover:** arrastrando el nodo.
- **Conectar:** tirar del punto de la derecha de un nodo hasta otro nodo.
- **Configurar:** clic en el nodo: se abre su panel a la derecha (ahí también se quita el nodo o
  sus conexiones).
- **Moverse por el lienzo:** dos dedos para desplazar, pellizcar o ⌘ + rueda para el zoom.
- **Guardar:** los cambios quedan en borrador hasta guardar (o *Descartar*).
- **Exportar:** copia el flujo en JSON al portapapeles, para guardarlo en el repo del proyecto
  (p. ej. `condx.workflow.json`) o llevarlo a otro proyecto con *Importar*.
- **Encender o apagar:** el interruptor del extremo derecho de la barra. Un flujo apagado se
  conserva con sus corridas pero no se puede ejecutar, tampoco un agente por el MCP.

Si al flujo le falta algo (una receta, un entorno que no existe, minutos en 0, un ciclo), lo dice
arriba y no deja ejecutarlo.

## Ejecutar y seguir una corrida

- Escribe el **commit o la rama** (por defecto `dev`) y toca **Ejecutar**.
- Los nodos corren de a uno, en el orden de las flechas. En la lista de **Corridas**, al elegir
  una, el lienzo muestra el estado de cada nodo (pendiente, corriendo, espera aprobación, listo,
  falló, saltado) y debajo, cada uno con su mensaje y un enlace a su trabajo.
- **Ir a aprobar:** cuando un nodo espera tu aprobación, lleva a la bandeja (también avisa el chip
  «por aprobar» de arriba).
- **Cancelar:** detiene la corrida y cancela el trabajo en curso.
- **Reintentar:** en una corrida que falló o se canceló, vuelve a empezar desde ese nodo (los
  anteriores no se repiten).
- La corrida usa el flujo **como estaba al ejecutarla**: editarlo después no la cambia.
- Si el daemon se reinicia, las corridas en curso siguen solas.

## Desde un entorno

Cada entorno tiene una pestaña **Flujos** con los flujos que lo tocan (algún nodo apunta a ese
entorno): su última corrida con el estado de cada paso, **Ejecutar** (con el commit o la rama de
arriba) y **Abrir** para editarlo. Debajo están las aprobaciones de ese entorno, así que un flujo se
sigue y se aprueba sin salir de la pestaña.

## Agentes (MCP)

Por el MCP, un agente puede **listar** los flujos (`list_workflows`), **ejecutar** uno
(`run_workflow`) y ver una corrida (`get_workflow_run`). Las aprobaciones de sus nodos esperan
igual que siempre: **solo la app aprueba**. Los nodos de aprobación siempre la piden, aunque
Configuración tenga «Sin aprobación» para ese tipo de entorno.
