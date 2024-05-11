use rand::prelude::*;
use rand_distr::Normal;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

struct WeatherStation {
    id: &'static str,
    mean_temperature: f64,
}

impl WeatherStation {
    fn measurement(&self) -> f64 {
        let normal_dist = Normal::new(self.mean_temperature, 10.0).unwrap();
        let mut rng = thread_rng();
        let measurement = normal_dist.sample(&mut rng);
        (measurement * 10.0).round() / 10.0
    }
}

fn main() -> std::io::Result<()> {
    let mut args = std::env::args();
    let Some(size) = args.nth(1) else {
        eprintln!("Usage: create_measurements <number of records to create>");
        std::process::exit(1);
    };

    let size = size
        .replace("_", "")
        .parse::<usize>()
        .expect("Invalid value for <number of records to create>");
    let file_name = args.next();
    let path = Path::new(file_name.as_deref().unwrap_or("measurements.txt"));
    let file = File::create(&path)?;
    let writer = BufWriter::new(file);
    generate_measurements(size, Arc::new(Mutex::new(writer)))?;
    Ok(())
}

fn generate_measurements<W: Write + Send + 'static>(
    size: usize,
    writer: Arc<Mutex<W>>,
) -> std::io::Result<()> {
    let start = Instant::now();
    let par_count = std::thread::available_parallelism().unwrap();
    println!(
        "Starting generating {} measurements with {} threads",
        size, par_count
    );
    let task_size = size / par_count;
    let mut tasks = vec![task_size; par_count.into()];
    for i in 0..(size % par_count) {
        tasks[i] += 1;
    }
    let tasks = tasks
        .into_iter()
        .map(|c| (c, writer.clone()))
        .map(|(count, a_m)| thread::spawn(move || create_measurements(count, a_m)))
        .collect::<Vec<_>>();
    tasks.into_iter().for_each(|t| t.join().unwrap().unwrap());
    println!(
        "Created file with {} measurements in {} ms",
        size,
        start.elapsed().as_millis()
    );
    Ok(())
}

const TMP_VEC_CAPACITY: usize = 50_000;
fn create_measurements<W: Write + Send>(
    count: usize,
    write_mutex: Arc<Mutex<W>>,
) -> std::io::Result<()> {
    let mut tmp_res = Vec::with_capacity(TMP_VEC_CAPACITY);
    let mut tmp_line = Vec::with_capacity(106);

    for _ in 0..count {
        let station = &STATIONS[thread_rng().gen_range(0..STATIONS.len())];

        writeln!(tmp_line, "{};{:.1}", station.id, station.measurement())?;
        if tmp_line.len() + tmp_res.len() > TMP_VEC_CAPACITY {
            write_mutex.lock().as_mut().unwrap().write_all(&tmp_res)?;
            tmp_res.clear();
        }
        tmp_res.write(&tmp_line)?;
        tmp_line.clear();
    }
    write_mutex.lock().as_mut().unwrap().write_all(&tmp_res)?;
    Ok(())
}
#[cfg(test)]
mod tests {

    use std::sync::{Arc, Mutex};

    use crate::generate_measurements;

    #[test]
    fn generate_10k() {
        let thing = Arc::new(Mutex::new(Vec::new()));
        generate_measurements(10_000, thing.clone()).unwrap();
        let res = String::from_utf8(thing.as_ref().lock().unwrap().clone())
            .expect("should be valid utf-8");
        assert_eq!(res.lines().count(), 10_000)
    }
    #[test]
    fn generate_random() {
        let thing = Arc::new(Mutex::new(Vec::new()));
        generate_measurements(77_123, thing.clone()).unwrap();
        let res = String::from_utf8(thing.as_ref().lock().unwrap().clone())
            .expect("should be valid utf-8");
        assert_eq!(res.lines().count(), 77_123)
    }
}
macro_rules! ws {
    ($id:expr,$measurement:expr) => {
        WeatherStation {
            id: $id,
            mean_temperature: $measurement,
        }
    };
}
const STATIONS: &[WeatherStation] = &[
    ws!("Abha", 18.0),
    ws!("Abidjan", 26.0),
    ws!("Abéché", 29.4),
    ws!("Accra", 26.4),
    ws!("Addis Ababa", 16.0),
    ws!("Adelaide", 17.3),
    ws!("Aden", 29.1),
    ws!("Ahvaz", 25.4),
    ws!("Albuquerque", 14.0),
    ws!("Alexandra", 11.0),
    ws!("Alexandria", 20.0),
    ws!("Algiers", 18.2),
    ws!("Alice Springs", 21.0),
    ws!("Almaty", 10.0),
    ws!("Amsterdam", 10.2),
    ws!("Anadyr", -6.9),
    ws!("Anchorage", 2.8),
    ws!("Andorra la Vella", 9.8),
    ws!("Ankara", 12.0),
    ws!("Antananarivo", 17.9),
    ws!("Antsiranana", 25.2),
    ws!("Arkhangelsk", 1.3),
    ws!("Ashgabat", 17.1),
    ws!("Asmara", 15.6),
    ws!("Assab", 30.5),
    ws!("Astana", 3.5),
    ws!("Athens", 19.2),
    ws!("Atlanta", 17.0),
    ws!("Auckland", 15.2),
    ws!("Austin", 20.7),
    ws!("Baghdad", 22.77),
    ws!("Baguio", 19.5),
    ws!("Baku", 15.1),
    ws!("Baltimore", 13.1),
    ws!("Bamako", 27.8),
    ws!("Bangkok", 28.6),
    ws!("Bangui", 26.0),
    ws!("Banjul", 26.0),
    ws!("Barcelona", 18.2),
    ws!("Bata", 25.1),
    ws!("Batumi", 14.0),
    ws!("Beijing", 12.9),
    ws!("Beirut", 20.9),
    ws!("Belgrade", 12.5),
    ws!("Belize City", 26.7),
    ws!("Benghazi", 19.9),
    ws!("Bergen", 7.7),
    ws!("Berlin", 10.3),
    ws!("Bilbao", 14.7),
    ws!("Birao", 26.5),
    ws!("Bishkek", 11.3),
    ws!("Bissau", 27.0),
    ws!("Blantyre", 22.2),
    ws!("Bloemfontein", 15.6),
    ws!("Boise", 11.4),
    ws!("Bordeaux", 14.2),
    ws!("Bosaso", 30.0),
    ws!("Boston", 10.9),
    ws!("Bouaké", 26.0),
    ws!("Bratislava", 10.5),
    ws!("Brazzaville", 25.0),
    ws!("Bridgetown", 27.0),
    ws!("Brisbane", 21.4),
    ws!("Brussels", 10.5),
    ws!("Bucharest", 10.8),
    ws!("Budapest", 11.3),
    ws!("Bujumbura", 23.8),
    ws!("Bulawayo", 18.9),
    ws!("Burnie", 13.1),
    ws!("Busan", 15.0),
    ws!("Cabo San Lucas", 23.9),
    ws!("Cairns", 25.0),
    ws!("Cairo", 21.4),
    ws!("Calgary", 4.4),
    ws!("Canberra", 13.1),
    ws!("Cape Town", 16.2),
    ws!("Changsha", 17.4),
    ws!("Charlotte", 16.1),
    ws!("Chiang Mai", 25.8),
    ws!("Chicago", 9.8),
    ws!("Chihuahua", 18.6),
    ws!("Chișinău", 10.2),
    ws!("Chittagong", 25.9),
    ws!("Chongqing", 18.6),
    ws!("Christchurch", 12.2),
    ws!("City of San Marino", 11.8),
    ws!("Colombo", 27.4),
    ws!("Columbus", 11.7),
    ws!("Conakry", 26.4),
    ws!("Copenhagen", 9.1),
    ws!("Cotonou", 27.2),
    ws!("Cracow", 9.3),
    ws!("Da Lat", 17.9),
    ws!("Da Nang", 25.8),
    ws!("Dakar", 24.0),
    ws!("Dallas", 19.0),
    ws!("Damascus", 17.0),
    ws!("Dampier", 26.4),
    ws!("Dar es Salaam", 25.8),
    ws!("Darwin", 27.6),
    ws!("Denpasar", 23.7),
    ws!("Denver", 10.4),
    ws!("Detroit", 10.0),
    ws!("Dhaka", 25.9),
    ws!("Dikson", -11.1),
    ws!("Dili", 26.6),
    ws!("Djibouti", 29.9),
    ws!("Dodoma", 22.7),
    ws!("Dolisie", 24.0),
    ws!("Douala", 26.7),
    ws!("Dubai", 26.9),
    ws!("Dublin", 9.8),
    ws!("Dunedin", 11.1),
    ws!("Durban", 20.6),
    ws!("Dushanbe", 14.7),
    ws!("Edinburgh", 9.3),
    ws!("Edmonton", 4.2),
    ws!("El Paso", 18.1),
    ws!("Entebbe", 21.0),
    ws!("Erbil", 19.5),
    ws!("Erzurum", 5.1),
    ws!("Fairbanks", -2.3),
    ws!("Fianarantsoa", 17.9),
    ws!("Flores,  Petén", 26.4),
    ws!("Frankfurt", 10.6),
    ws!("Fresno", 17.9),
    ws!("Fukuoka", 17.0),
    ws!("Gabès", 19.5),
    ws!("Gaborone", 21.0),
    ws!("Gagnoa", 26.0),
    ws!("Gangtok", 15.2),
    ws!("Garissa", 29.3),
    ws!("Garoua", 28.3),
    ws!("George Town", 27.9),
    ws!("Ghanzi", 21.4),
    ws!("Gjoa Haven", -14.4),
    ws!("Guadalajara", 20.9),
    ws!("Guangzhou", 22.4),
    ws!("Guatemala City", 20.4),
    ws!("Halifax", 7.5),
    ws!("Hamburg", 9.7),
    ws!("Hamilton", 13.8),
    ws!("Hanga Roa", 20.5),
    ws!("Hanoi", 23.6),
    ws!("Harare", 18.4),
    ws!("Harbin", 5.0),
    ws!("Hargeisa", 21.7),
    ws!("Hat Yai", 27.0),
    ws!("Havana", 25.2),
    ws!("Helsinki", 5.9),
    ws!("Heraklion", 18.9),
    ws!("Hiroshima", 16.3),
    ws!("Ho Chi Minh City", 27.4),
    ws!("Hobart", 12.7),
    ws!("Hong Kong", 23.3),
    ws!("Honiara", 26.5),
    ws!("Honolulu", 25.4),
    ws!("Houston", 20.8),
    ws!("Ifrane", 11.4),
    ws!("Indianapolis", 11.8),
    ws!("Iqaluit", -9.3),
    ws!("Irkutsk", 1.0),
    ws!("Istanbul", 13.9),
    ws!("İzmir", 17.9),
    ws!("Jacksonville", 20.3),
    ws!("Jakarta", 26.7),
    ws!("Jayapura", 27.0),
    ws!("Jerusalem", 18.3),
    ws!("Johannesburg", 15.5),
    ws!("Jos", 22.8),
    ws!("Juba", 27.8),
    ws!("Kabul", 12.1),
    ws!("Kampala", 20.0),
    ws!("Kandi", 27.7),
    ws!("Kankan", 26.5),
    ws!("Kano", 26.4),
    ws!("Kansas City", 12.5),
    ws!("Karachi", 26.0),
    ws!("Karonga", 24.4),
    ws!("Kathmandu", 18.3),
    ws!("Khartoum", 29.9),
    ws!("Kingston", 27.4),
    ws!("Kinshasa", 25.3),
    ws!("Kolkata", 26.7),
    ws!("Kuala Lumpur", 27.3),
    ws!("Kumasi", 26.0),
    ws!("Kunming", 15.7),
    ws!("Kuopio", 3.4),
    ws!("Kuwait City", 25.7),
    ws!("Kyiv", 8.4),
    ws!("Kyoto", 15.8),
    ws!("La Ceiba", 26.2),
    ws!("La Paz", 23.7),
    ws!("Lagos", 26.8),
    ws!("Lahore", 24.3),
    ws!("Lake Havasu City", 23.7),
    ws!("Lake Tekapo", 8.7),
    ws!("Las Palmas de Gran Canaria", 21.2),
    ws!("Las Vegas", 20.3),
    ws!("Launceston", 13.1),
    ws!("Lhasa", 7.6),
    ws!("Libreville", 25.9),
    ws!("Lisbon", 17.5),
    ws!("Livingstone", 21.8),
    ws!("Ljubljana", 10.9),
    ws!("Lodwar", 29.3),
    ws!("Lomé", 26.9),
    ws!("London", 11.3),
    ws!("Los Angeles", 18.6),
    ws!("Louisville", 13.9),
    ws!("Luanda", 25.8),
    ws!("Lubumbashi", 20.8),
    ws!("Lusaka", 19.9),
    ws!("Luxembourg City", 9.3),
    ws!("Lviv", 7.8),
    ws!("Lyon", 12.5),
    ws!("Madrid", 15.0),
    ws!("Mahajanga", 26.3),
    ws!("Makassar", 26.7),
    ws!("Makurdi", 26.0),
    ws!("Malabo", 26.3),
    ws!("Malé", 28.0),
    ws!("Managua", 27.3),
    ws!("Manama", 26.5),
    ws!("Mandalay", 28.0),
    ws!("Mango", 28.1),
    ws!("Manila", 28.4),
    ws!("Maputo", 22.8),
    ws!("Marrakesh", 19.6),
    ws!("Marseille", 15.8),
    ws!("Maun", 22.4),
    ws!("Medan", 26.5),
    ws!("Mek'ele", 22.7),
    ws!("Melbourne", 15.1),
    ws!("Memphis", 17.2),
    ws!("Mexicali", 23.1),
    ws!("Mexico City", 17.5),
    ws!("Miami", 24.9),
    ws!("Milan", 13.0),
    ws!("Milwaukee", 8.9),
    ws!("Minneapolis", 7.8),
    ws!("Minsk", 6.7),
    ws!("Mogadishu", 27.1),
    ws!("Mombasa", 26.3),
    ws!("Monaco", 16.4),
    ws!("Moncton", 6.1),
    ws!("Monterrey", 22.3),
    ws!("Montreal", 6.8),
    ws!("Moscow", 5.8),
    ws!("Mumbai", 27.1),
    ws!("Murmansk", 0.6),
    ws!("Muscat", 28.0),
    ws!("Mzuzu", 17.7),
    ws!("N'Djamena", 28.3),
    ws!("Naha", 23.1),
    ws!("Nairobi", 17.8),
    ws!("Nakhon Ratchasima", 27.3),
    ws!("Napier", 14.6),
    ws!("Napoli", 15.9),
    ws!("Nashville", 15.4),
    ws!("Nassau", 24.6),
    ws!("Ndola", 20.3),
    ws!("New Delhi", 25.0),
    ws!("New Orleans", 20.7),
    ws!("New York City", 12.9),
    ws!("Ngaoundéré", 22.0),
    ws!("Niamey", 29.3),
    ws!("Nicosia", 19.7),
    ws!("Niigata", 13.9),
    ws!("Nouadhibou", 21.3),
    ws!("Nouakchott", 25.7),
    ws!("Novosibirsk", 1.7),
    ws!("Nuuk", -1.4),
    ws!("Odesa", 10.7),
    ws!("Odienné", 26.0),
    ws!("Oklahoma City", 15.9),
    ws!("Omaha", 10.6),
    ws!("Oranjestad", 28.1),
    ws!("Oslo", 5.7),
    ws!("Ottawa", 6.6),
    ws!("Ouagadougou", 28.3),
    ws!("Ouahigouya", 28.6),
    ws!("Ouarzazate", 18.9),
    ws!("Oulu", 2.7),
    ws!("Palembang", 27.3),
    ws!("Palermo", 18.5),
    ws!("Palm Springs", 24.5),
    ws!("Palmerston North", 13.2),
    ws!("Panama City", 28.0),
    ws!("Parakou", 26.8),
    ws!("Paris", 12.3),
    ws!("Perth", 18.7),
    ws!("Petropavlovsk-Kamchatsky", 1.9),
    ws!("Philadelphia", 13.2),
    ws!("Phnom Penh", 28.3),
    ws!("Phoenix", 23.9),
    ws!("Pittsburgh", 10.8),
    ws!("Podgorica", 15.3),
    ws!("Pointe-Noire", 26.1),
    ws!("Pontianak", 27.7),
    ws!("Port Moresby", 26.9),
    ws!("Port Sudan", 28.4),
    ws!("Port Vila", 24.3),
    ws!("Port-Gentil", 26.0),
    ws!("Portland (OR)", 12.4),
    ws!("Porto", 15.7),
    ws!("Prague", 8.4),
    ws!("Praia", 24.4),
    ws!("Pretoria", 18.2),
    ws!("Pyongyang", 10.8),
    ws!("Rabat", 17.2),
    ws!("Rangpur", 24.4),
    ws!("Reggane", 28.3),
    ws!("Reykjavík", 4.3),
    ws!("Riga", 6.2),
    ws!("Riyadh", 26.0),
    ws!("Rome", 15.2),
    ws!("Roseau", 26.2),
    ws!("Rostov-on-Don", 9.9),
    ws!("Sacramento", 16.3),
    ws!("Saint Petersburg", 5.8),
    ws!("Saint-Pierre", 5.7),
    ws!("Salt Lake City", 11.6),
    ws!("San Antonio", 20.8),
    ws!("San Diego", 17.8),
    ws!("San Francisco", 14.6),
    ws!("San Jose", 16.4),
    ws!("San José", 22.6),
    ws!("San Juan", 27.2),
    ws!("San Salvador", 23.1),
    ws!("Sana'a", 20.0),
    ws!("Santo Domingo", 25.9),
    ws!("Sapporo", 8.9),
    ws!("Sarajevo", 10.1),
    ws!("Saskatoon", 3.3),
    ws!("Seattle", 11.3),
    ws!("Ségou", 28.0),
    ws!("Seoul", 12.5),
    ws!("Seville", 19.2),
    ws!("Shanghai", 16.7),
    ws!("Singapore", 27.0),
    ws!("Skopje", 12.4),
    ws!("Sochi", 14.2),
    ws!("Sofia", 10.6),
    ws!("Sokoto", 28.0),
    ws!("Split", 16.1),
    ws!("St. John's", 5.0),
    ws!("St. Louis", 13.9),
    ws!("Stockholm", 6.6),
    ws!("Surabaya", 27.1),
    ws!("Suva", 25.6),
    ws!("Suwałki", 7.2),
    ws!("Sydney", 17.7),
    ws!("Tabora", 23.0),
    ws!("Tabriz", 12.6),
    ws!("Taipei", 23.0),
    ws!("Tallinn", 6.4),
    ws!("Tamale", 27.9),
    ws!("Tamanrasset", 21.7),
    ws!("Tampa", 22.9),
    ws!("Tashkent", 14.8),
    ws!("Tauranga", 14.8),
    ws!("Tbilisi", 12.9),
    ws!("Tegucigalpa", 21.7),
    ws!("Tehran", 17.0),
    ws!("Tel Aviv", 20.0),
    ws!("Thessaloniki", 16.0),
    ws!("Thiès", 24.0),
    ws!("Tijuana", 17.8),
    ws!("Timbuktu", 28.0),
    ws!("Tirana", 15.2),
    ws!("Toamasina", 23.4),
    ws!("Tokyo", 15.4),
    ws!("Toliara", 24.1),
    ws!("Toluca", 12.4),
    ws!("Toronto", 9.4),
    ws!("Tripoli", 20.0),
    ws!("Tromsø", 2.9),
    ws!("Tucson", 20.9),
    ws!("Tunis", 18.4),
    ws!("Ulaanbaatar", -0.4),
    ws!("Upington", 20.4),
    ws!("Ürümqi", 7.4),
    ws!("Vaduz", 10.1),
    ws!("Valencia", 18.3),
    ws!("Valletta", 18.8),
    ws!("Vancouver", 10.4),
    ws!("Veracruz", 25.4),
    ws!("Vienna", 10.4),
    ws!("Vientiane", 25.9),
    ws!("Villahermosa", 27.1),
    ws!("Vilnius", 6.0),
    ws!("Virginia Beach", 15.8),
    ws!("Vladivostok", 4.9),
    ws!("Warsaw", 8.5),
    ws!("Washington, D.C.", 14.6),
    ws!("Wau", 27.8),
    ws!("Wellington", 12.9),
    ws!("Whitehorse", -0.1),
    ws!("Wichita", 13.9),
    ws!("Willemstad", 28.0),
    ws!("Winnipeg", 3.0),
    ws!("Wrocław", 9.6),
    ws!("Xi'an", 14.1),
    ws!("Yakutsk", -8.8),
    ws!("Yangon", 27.5),
    ws!("Yaoundé", 23.8),
    ws!("Yellowknife", -4.3),
    ws!("Yerevan", 12.4),
    ws!("Yinchuan", 9.0),
    ws!("Zagreb", 10.7),
    ws!("Zanzibar City", 26.0),
    ws!("Zürich", 9.3),
];
