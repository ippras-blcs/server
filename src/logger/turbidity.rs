use super::Writer;
use crate::{SETTINGS, turbidity::Message};
use anyhow::Result;
use arrow::{
    array::{RecordBatch, TimestampMillisecondArray, UInt16Array, UInt64Array},
    datatypes::{DataType, Field, Schema, TimeUnit},
};
use object_store::local::LocalFileSystem;
use std::sync::Arc;
use tokio::{
    select,
    sync::{broadcast, mpsc},
    task::{Builder, JoinHandle},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, instrument, warn};

const TURBIDITY: &str = "turbidity";
const CHANNEL_BUFFER: usize = 64;

#[instrument(err)]
pub async fn run(
    receiver: broadcast::Receiver<Message>,
    cancellation: CancellationToken,
) -> Result<()> {
    let channel = mpsc::channel(CHANNEL_BUFFER);
    let reader = reader(receiver, channel.0, cancellation.clone())?;
    let writer = writer(channel.1, cancellation.clone())?;
    select! {
        result = reader => result?,
        result = writer => result?,
    }
    Ok(())
}

fn reader(
    receiver: broadcast::Receiver<Message>,
    sender: mpsc::Sender<Message>,
    cancellation: CancellationToken,
) -> Result<JoinHandle<()>> {
    Ok(Builder::new().name("reader").spawn(Box::pin(async move {
        select! {
            biased;
            _ = cancellation.cancelled() => warn!("logger turbidity reader cancelled"),
            _ = read(receiver, sender) => warn!("logger turbidity reader returned"),
        }
    }))?)
}

#[instrument(err)]
pub(crate) async fn read(
    mut receiver: broadcast::Receiver<Message>,
    sender: mpsc::Sender<Message>,
) -> Result<()> {
    loop {
        let message = receiver.recv().await?;
        sender.send(message).await?;
    }
}

fn writer(
    receiver: mpsc::Receiver<Message>,
    cancellation: CancellationToken,
) -> Result<JoinHandle<()>> {
    Ok(Builder::new().name("writer").spawn(Box::pin(async move {
        select! {
            biased;
            _ = cancellation.cancelled() => warn!("logger turbidity writer cancelled"),
            _ = write(receiver) => warn!("logger turbidity writer returned"),
        }
    }))?)
}

#[instrument(err)]
async fn write(mut receiver: mpsc::Receiver<Message>) -> Result<()> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("Identifier", DataType::UInt64, false),
        Field::new(
            "Timestamp",
            DataType::Timestamp(TimeUnit::Millisecond, None),
            false,
        ),
        Field::new("Turbidity", DataType::UInt16, false),
    ]));
    let store = Arc::new(LocalFileSystem::new());
    let builder = Writer::builder()
        .schema(schema.clone())
        .store(store)
        .folder(TURBIDITY);
    let mut maybe_writer = None;
    while let Some(Message {
        identifier,
        date_time,
        value,
    }) = receiver.recv().await
    {
        let writer = match &mut maybe_writer {
            Some(writer) => writer,
            None => maybe_writer.insert(builder.clone().date_time(date_time).build()?),
        };
        debug!(?writer);
        let count = 1;
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(UInt64Array::from_value(identifier, count)),
                Arc::new(TimestampMillisecondArray::from_value(
                    date_time.timestamp_millis(),
                    count,
                )),
                Arc::new(UInt16Array::from_value(value, count)),
            ],
        )?;
        writer.write(&batch).await?;
        // Check for flush
        if writer.in_progress_rows() >= SETTINGS.turbidity.flush() {
            info!("Flush {}", writer.in_progress_rows());
            writer.flush().await?
        }
        // Check for writer
        if writer.flushed_row_groups().len() >= SETTINGS.turbidity.finish {
            info!("Finish {}", writer.flushed_row_groups().len());
            writer.finish().await?;
            maybe_writer.take();
        }
    }
    Ok(())
}

// /// parquet
// pub async fn _run(receiver: &mut broadcast::Receiver<Message>) -> Result<()> {
//     let schema = Arc::new(Schema::new(vec![
//         Field::new("Identifier", DataType::UInt64, false),
//         Field::new("Turbidity", DataType::UInt16, false),
//         Field::new(
//             "Timestamp",
//             DataType::Timestamp(TimeUnit::Millisecond, None),
//             false,
//         ),
//     ]));
//     let store = Arc::new(LocalFileSystem::new());
//     let builder = Writer::builder()
//         .schema(schema.clone())
//         .store(store)
//         .folder(TURBIDITY);
//     let mut maybe_writer = None;
//     loop {
//         if let Some(writer) = &maybe_writer {
//             debug!(?writer);
//         }
//         let Message {
//             identifier,
//             value,
//             date_time,
//         } = match receiver.recv().await {
//             Ok(message) => message,
//             Err(error @ RecvError::Lagged(_)) => {
//                 warn!(%error);
//                 continue;
//             }
//             Err(error) => Err(error)?,
//         };
//         let writer = match &mut maybe_writer {
//             Some(writer) => writer,
//             None => maybe_writer.insert(builder.clone().date_time(date_time).build()?),
//         };
//         let count = 1;
//         let batch = RecordBatch::try_new(
//             schema.clone(),
//             vec![
//                 Arc::new(UInt64Array::from_value(identifier, count)),
//                 Arc::new(UInt16Array::from_value(value, count)),
//                 Arc::new(TimestampMillisecondArray::from_value(
//                     date_time.timestamp_millis(),
//                     count,
//                 )),
//             ],
//         )?;
//         writer.write(&batch).await?;
//         // Check for flush
//         if writer.in_progress_rows() >= FLUSH {
//             info!("Flush {}", writer.in_progress_rows());
//             writer.flush().await?
//         }
//         // Check for writer
//         if writer.flushed_row_groups().len() >= WRITE {
//             info!("Close {}", writer.flushed_row_groups().len());
//             writer.finish().await?;
//             maybe_writer.take();
//         }
//     }
// }
