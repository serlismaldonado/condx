# El día a día: devbox y desechables

Aquí casi nada pide aprobación.

| Quiero… | Receta | Entorno |
| --- | --- | --- |
| Correr una función o un relleno | Correr una función / Correr un relleno | local o un desechable |
| Respaldar | Respaldo (export) | cualquiera |
| Restaurar un respaldo | Restaurar respaldo | local pide aprobación; un desechable, no |
| Reiniciar el backend o reconstruir el deployer | Reiniciar el backend / Reconstruir la imagen del deployer | local |
| Probar algo sin tocar nada real | Crear backend desechable → lo que quieras → Destruir backend desechable | nuevo |

> En el devbox, «Desplegar un commit» dura poco: el deployer vuelve a desplegar tu checkout con el próximo archivo que guardes.
